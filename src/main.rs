//! `omoi`: local memory for AI agents. One binary, one SQLite file.

use anyhow::{Context, Result, bail};
use clap::{Arg, ArgAction, Args, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use omoikane::buscar::{self, SearchOptions};
use omoikane::memoria::{self, Scope, Changes, Memory, NewMemory, Kind, Version};
use omoikane::embeber::Modelo;
use omoikane::{db, idioma, modelos, proyecto, recall, tr, vectores};
use rusqlite::Connection;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Local memory for AI coding agents: keeps what they learn and gives it back when it matters.
#[derive(Parser)]
#[command(name = "omoi", version)]
struct Cli {
    #[command(flatten)]
    comun: Common,
    #[command(subcommand)]
    orden: Commands,
}

#[derive(Args)]
struct Common {
    /// Database file (default: OMOIKANE_DB, or omoikane.db in OMOIKANE_HOME, ~/.omoikane by default).
    #[arg(long, global = true, value_name = "FILE")]
    db: Option<PathBuf>,
    /// Folder whose project to use (default: the current one).
    #[arg(long, global = true, value_name = "DIR")]
    dir: Option<PathBuf>,
    /// JSON output, for other programs.
    #[arg(long, global = true)]
    json: bool,
}

// `omoi diary`, with its options apart: inside `Commands`, the function that builds clap's help
// grew so much that it overflowed the stack (1 MB on Windows) in the debug build.
#[derive(Args)]
struct DiaryArgs {
    /// How many.
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: usize,
    /// Sessions that ended on or after this date (2026-11-03, or 7d for the last 7 days).
    #[arg(long, value_name = "DATE")]
    since: Option<String>,
    /// Sessions that started on or before this date.
    #[arg(long, value_name = "DATE")]
    until: Option<String>,
    /// Only sessions that touched a file whose path contains this.
    #[arg(long, value_name = "PATH")]
    file: Option<String>,
    /// From every project (of this space, if the project is in one).
    #[arg(long)]
    all: bool,
    /// From this project instead: its folder, or its name or part of it.
    #[arg(long, value_name = "PROJECT", conflicts_with = "all")]
    project: Option<String>,
    /// From this space instead.
    #[arg(long, value_name = "SPACE", conflicts_with_all = ["all", "project"])]
    space: Option<String>,
}

// `omoi update`, with its options apart (like `omoi diary`): inside `Commands`, the function that builds
// clap's help overflowed the stack in the debug build.
#[derive(Args)]
struct UpdateArgs {
    /// Memory id (its start is enough).
    id: String,
    /// New title.
    #[arg(short, long)]
    title: Option<String>,
    /// New content ("-" to read it from standard input).
    #[arg(short, long)]
    content: Option<String>,
    /// New kind.
    #[arg(short, long)]
    kind: Option<Kind>,
    /// Replace the paths with these (repeatable).
    #[arg(long = "path", value_name = "PATH")]
    paths: Vec<String>,
    /// Remove all paths.
    #[arg(long, conflicts_with = "paths")]
    no_paths: bool,
    /// Replace the tags with these (repeatable).
    #[arg(long = "tag", value_name = "TAG")]
    tags: Vec<String>,
    /// Remove all tags.
    #[arg(long, conflicts_with = "tags")]
    no_tags: bool,
    /// Pin it: always injected in its project.
    #[arg(long, conflicts_with = "unpin")]
    pin: bool,
    /// Unpin it.
    #[arg(long)]
    unpin: bool,
    /// Move it to the project of this folder.
    #[arg(long, value_name = "DIR", conflicts_with = "global")]
    project: Option<PathBuf>,
    /// Make it global: it applies to every project.
    #[arg(long)]
    global: bool,
    /// Move it to this space: it applies to every project in it.
    #[arg(long, value_name = "SPACE", conflicts_with_all = ["global", "project"])]
    space: Option<String>,
    /// Make it a trade memory (repeatable): every project of that trade sees it. A personal memory keeps its id, history and verdicts; a client's (in a space) becomes a NEW trade memory and the original stays.
    #[arg(long = "trade", value_name = "TRADE", conflicts_with_all = ["space", "project", "no_trade"])]
    trades: Vec<String>,
    /// It stops being a trade memory (it becomes an ordinary global memory, not back to its project).
    #[arg(long)]
    no_trade: bool,
    /// On a trade memory, who it is for: client (projects in a space), personal (projects in no space) or both.
    #[arg(long = "for", value_name = "WHO", conflicts_with = "no_trade")]
    para: Option<String>,
    /// With a trade memory: save even if the leak check flags something you checked is a false positive (never an absolute path or a repository URL).
    #[arg(long)]
    allow_leaks: bool,
    /// Why it changes (kept with the previous version).
    #[arg(short, long)]
    reason: Option<String>,
    /// Exact text to replace in the content; it has to appear exactly once. Pair each one with a --with (repeatable).
    #[arg(long, value_name = "FIND")]
    replace: Vec<String>,
    /// What goes in place of the matching --replace (repeatable, in the same order).
    #[arg(long, value_name = "TEXT", requires = "replace")]
    with: Vec<String>,
    /// Text added at the end of the content, after a blank line (one more step in a procedure).
    #[arg(long, value_name = "TEXT")]
    append: Option<String>,
    #[command(flatten)]
    datos: UpdateData,
}

// `omoi decisions` and `omoi file-history`, apart for the same reason as `DiaryArgs`.
#[derive(Args)]
struct DecisionsArgs {
    /// Only the blocking ones (work that can't go on without the answer).
    #[arg(long)]
    blocking: bool,
    /// How many.
    #[arg(short = 'n', long, default_value_t = 50)]
    limit: usize,
    /// From every project (of this space, if the project is in one).
    #[arg(long)]
    all: bool,
    /// From this project instead: its folder, or its name or part of it.
    #[arg(long, value_name = "PROJECT", conflicts_with = "all")]
    project: Option<String>,
    /// From this space instead.
    #[arg(long, value_name = "SPACE", conflicts_with_all = ["all", "project"])]
    space: Option<String>,
    /// Only those with this tag too (repeatable: all of them).
    #[arg(long = "tag", value_name = "TAG")]
    tags: Vec<String>,
    /// Resolve this decision instead of listing: its answer is appended, the `open` and `blocking` tags go and `resolved` is added.
    #[arg(long, value_name = "ID", requires = "answer", conflicts_with_all = ["blocking", "all", "project", "space", "tags"])]
    resolve: Option<String>,
    /// With --resolve: the user's answer.
    #[arg(long, value_name = "TEXT", requires = "resolve")]
    answer: Option<String>,
    /// With --resolve: why (kept with the previous version).
    #[arg(short, long, requires = "resolve")]
    reason: Option<String>,
    /// With --resolve, on a trade decision: save the answer even if the leak check flags a false positive (never an absolute path or a repository URL).
    #[arg(long, requires = "resolve")]
    allow_leaks: bool,
}

// The data of `remember` and `update`, apart for the same reason as `DiaryArgs`.
#[derive(Args)]
struct NewData {
    /// Structured data for programs, as JSON: kept with the memory, out of search and recall.
    #[arg(long, value_name = "JSON")]
    data: Option<String>,
    /// What it is based on, KIND:DETAIL (repeatable): user-said, code-read, tested, doc (doc:URL text) or inferred. E.g. --evidence "tested:cargo test, 83 passed".
    #[arg(long, value_name = "KIND:DETAIL")]
    evidence: Vec<String>,
    /// With a review:YYYY-MM-DD tag: what to check that day ("the API token expires").
    #[arg(long, value_name = "TEXT")]
    review_reason: Option<String>,
}

#[derive(Args)]
struct UpdateData {
    /// Replace its structured data with this JSON (out of search and recall).
    #[arg(long, value_name = "JSON")]
    data: Option<String>,
    /// Remove its structured data.
    #[arg(long, conflicts_with = "data")]
    no_data: bool,
    /// More evidence, KIND:DETAIL (repeatable; added to what it had, no new version): user-said, code-read, tested, doc or inferred.
    #[arg(long, value_name = "KIND:DETAIL")]
    evidence: Vec<String>,
    /// What to check when its review date (tag review:YYYY-MM-DD) arrives (no new version).
    #[arg(long, value_name = "TEXT")]
    review_reason: Option<String>,
}

#[derive(Args)]
struct ListArgs {
    /// Tag the memories must have (repeatable: all of them). At least one.
    #[arg(long = "tag", value_name = "TAG", required = true)]
    tags: Vec<String>,
    /// Only this kind.
    #[arg(short, long)]
    kind: Option<Kind>,
    /// At most this many (all, by default).
    #[arg(short = 'n', long)]
    limit: Option<usize>,
    /// From every project (of this space, if the project is in one).
    #[arg(long)]
    all: bool,
    /// From this project instead: its folder, or its name or part of it.
    #[arg(long, value_name = "PROJECT", conflicts_with = "all")]
    project: Option<String>,
    /// From this space instead.
    #[arg(long, value_name = "SPACE", conflicts_with_all = ["all", "project"])]
    space: Option<String>,
}

/// `omoi verify`: a person's mark (see `verificado`). Apart, so as not to enlarge `Commands`.
#[derive(Args)]
struct VerifyArgs {
    /// Memory id (its start is enough).
    id: String,
    /// Who verifies it. By default, your name in the sync (OMOIKANE_AUTHOR or the one you joined with), or the system user.
    #[arg(long, value_name = "NAME")]
    by: Option<String>,
    /// Remove its verification instead.
    #[arg(long, conflicts_with = "by")]
    unverify: bool,
    /// Don't ask (for a program you drive, after you approved it there). Without it, omoi asks in a terminal and refuses elsewhere. It always refuses inside a coding agent's shell.
    #[arg(short, long)]
    yes: bool,
}

#[derive(Args)]
struct FileHistoryArgs {
    /// Files, relative to the repository or absolute. Without any (or with -), one per line from the standard input, such as the output of `git diff --name-only`.
    #[arg(value_name = "FILE")]
    files: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Save a memory. The content goes as an argument or through standard input.
    Remember {
        /// fact, decision, gotcha, preference or procedure.
        #[arg(short, long)]
        kind: Kind,
        /// One line saying what it is about.
        #[arg(short, long)]
        title: String,
        /// What, why and where. Without it (or with "-"), it is read from standard input.
        content: Option<String>,
        /// File or folder of the project it refers to (repeatable).
        #[arg(long = "path", value_name = "PATH")]
        paths: Vec<String>,
        /// Free tag (repeatable). `review:YYYY-MM-DD` flags it for review that day.
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
        /// Always injected in its project.
        #[arg(long)]
        pinned: bool,
        /// Applies to every project, not just this one.
        #[arg(long)]
        global: bool,
        /// Applies to every project of a space: this project's (`--space`) or the named one (`--space=<name>`).
        #[arg(long, value_name = "SPACE", num_args = 0..=1, require_equals = true, default_missing_value = "", conflicts_with = "global")]
        space: Option<String>,
        /// A trade memory (android, web, wordpress…; repeatable): every project of that trade sees it, of any client, and no other. Only knowledge of the stack, nothing of a project or client.
        #[arg(long = "trade", value_name = "TRADE", conflicts_with_all = ["space", "global"])]
        trades: Vec<String>,
        /// With --trade, who it is for: client (only projects in a space, a client's), personal (only projects in no space) or both (the default).
        #[arg(long = "for", value_name = "WHO", requires = "trades")]
        para: Option<String>,
        /// Who saves it (for example "claude-code:<session>").
        #[arg(long, default_value = "cli")]
        source: String,
        /// Save it even if a very similar memory already exists (it does not skip the leak check of --trade).
        #[arg(short, long)]
        force: bool,
        /// With --trade: save even if the leak check flags something you checked is a false positive (never an absolute path or a repository URL).
        #[arg(long, requires = "trades")]
        allow_leaks: bool,
        /// A stable key (e.g. kumi-profile): creates or updates the one memory with this key at its level (this project, or the one --space, --trade or --global says), with no duplicate check.
        #[arg(long, value_name = "KEY")]
        key: Option<String>,
        #[command(flatten)]
        datos: NewData,
    },
    /// Search the memories of this project and the global ones.
    Search {
        /// What you are looking for, in plain words.
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// Only this kind.
        #[arg(short, long)]
        kind: Option<Kind>,
        /// Only memories with this tag (repeatable).
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
        /// How many results.
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// In every project (of this space, if the project is in one).
        #[arg(long)]
        all: bool,
        /// In this project instead: its folder, or its name or part of it.
        #[arg(long, value_name = "PROJECT", conflicts_with = "all")]
        project: Option<String>,
        /// In this space instead.
        #[arg(long, value_name = "SPACE", conflicts_with_all = ["all", "project"])]
        space: Option<String>,
    },
    /// Show a whole memory (the start of the id is enough).
    Get {
        /// Memory id (its start is enough).
        #[arg(required_unless_present = "key")]
        id: Option<String>,
        /// Instead of the id, the key it was saved with (`remember --key`): the one this project sees.
        #[arg(long, value_name = "KEY", conflicts_with = "id")]
        key: Option<String>,
        /// With --key: every memory with that key this project sees, most specific first (project, space, trade, global), each with its scope and data, unmerged.
        #[arg(long, requires = "key")]
        resolve: bool,
    },
    /// Change a memory. The previous version is kept in its history.
    Update(UpdateArgs),
    /// Forget a memory: it goes to the trash with its history (`omoi restore` brings it back; emptied after 30 days).
    Forget {
        /// Memory id (its start is enough).
        id: String,
        /// Don't ask. Without it, omoi asks in a terminal and refuses elsewhere.
        #[arg(short, long)]
        yes: bool,
        /// Skip the trash: delete it for good.
        #[arg(long)]
        purge: bool,
    },
    /// Bring back a memory from the trash, on this or any synced computer (without an id, list the trash seen from here).
    Restore {
        /// Memory id (its first 4 characters or more).
        id: Option<String>,
    },
    /// What is in the trash, seen from here (from a client's project, its space and the global ones): memories forgotten on this or another computer, kept 30 days (OMOIKANE_TRASH_DAYS).
    Trash {
        /// Empty it now (what is seen from here).
        #[arg(long)]
        empty: bool,
        /// Don't ask before emptying.
        #[arg(short, long)]
        yes: bool,
    },
    /// Previous versions of a memory.
    History {
        /// Memory id (its start is enough).
        id: String,
    },
    /// Mark a memory as verified by you, a person (agents write the rest). Recall, search and get show it; editing its text removes it. Agents can't do this.
    Verify(VerifyArgs),
    /// The latest saved or changed memories.
    Recent {
        /// How many.
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// Only this kind.
        #[arg(short, long)]
        kind: Option<Kind>,
        /// From every project (of this space, if the project is in one).
        #[arg(long)]
        all: bool,
        /// From this project instead: its folder, or its name or part of it.
        #[arg(long, value_name = "PROJECT", conflicts_with = "all")]
        project: Option<String>,
        /// From this space instead.
        #[arg(long, value_name = "SPACE", conflicts_with_all = ["all", "project"])]
        space: Option<String>,
    },
    /// The session diary: one line per session (when, which project, files touched, memories recalled, applied and saved), newest first. Only on this computer.
    Diary(DiaryArgs),
    /// Turn a memory into a skill of this repository (`.agents/skills/<name>/SKILL.md` and `.claude/skills/<name>/SKILL.md`), so every agent and teammate gets it once committed. Without an id, lists the repository's skills from Omoikane and which are outdated or edited by hand.
    Skill {
        /// The memory: its id (or the start of it), or a few words to find it ("publish release"). Without it, lists the repository's skills and the memories that could be skills, with the command for each.
        id: Option<String>,
        /// Only the memories that could be skills (procedures, and memories the agent applied in 2 sessions or more), with their ids.
        #[arg(long, conflicts_with_all = ["id", "refresh"])]
        suggest: bool,
        /// Bring up to date every skill whose memory changed since (not the ones edited by hand).
        #[arg(long, conflicts_with = "id")]
        refresh: bool,
        /// Where to write it: both (default), agents (`.agents/skills`) or claude (`.claude/skills`).
        #[arg(long, value_name = "TARGET")]
        to: Option<String>,
        /// Replace a skill edited by hand or not from Omoikane, and write it even if it looks like it holds a secret.
        #[arg(long)]
        force: bool,
        /// Say what it would write without touching anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// The tags here that are variants of each other (`idea` and `ideas`, `test` and `tests`), with how many memories carry each. With --merge, one replaces the other in every memory here.
    Tags {
        /// Replace the tag FROM with TO in every memory here that carries it (each change keeps its history).
        #[arg(long, num_args = 2, value_names = ["FROM", "TO"])]
        merge: Option<Vec<String>>,
        /// In every project (of this space, if the project is in one).
        #[arg(long)]
        all: bool,
        /// In this project instead: its folder, or its name or part of it.
        #[arg(long, value_name = "PROJECT", conflicts_with = "all")]
        project: Option<String>,
        /// In this space instead.
        #[arg(long, value_name = "SPACE", conflicts_with_all = ["all", "project"])]
        space: Option<String>,
    },
    /// Every memory with these tags in this project (its space's and the global ones), newest first, with no search or ranking: for programs that save something under a tag and must always find it again.
    List(ListArgs),
    /// Open decisions waiting for the user (kind decision, tag `open`): blocking ones (tag `blocking`) first, then the ones for later.
    Decisions(DecisionsArgs),
    /// What the session diary knows about some files of this project: in how many sessions each changed, when last, which files change with it and which gotchas mention it. For code such as a risk gate; only on this computer.
    FileHistory(FileHistoryArgs),
    /// The project's own Markdown documentation (a docs/ folder, a wiki, an Obsidian vault), read-only: recall brings the section that fits a prompt. Without an action, the list.
    Docs {
        #[command(subcommand)]
        accion: Option<DocsAction>,
    },
    /// Projects with memories (without an action, the list): move one to its new folder, or merge two.
    Projects {
        #[command(subcommand)]
        accion: Option<ProjectsAction>,
    },
    /// Spaces: groups of projects (a client, a product with several repositories) whose memories don't reach other spaces.
    #[command(alias = "spaces")]
    Space {
        #[command(subcommand)]
        accion: Option<SpaceAction>,
    },
    /// Trades: a project's stack (android, web, wordpress…), detected from its files. Trade memories reach every project of their trade. Without an action, this project's.
    #[command(alias = "trade")]
    Trades {
        #[command(subcommand)]
        accion: Option<TradesAction>,
    },
    /// Omoikane's own update: check now, or turn the daily automatic update on or off.
    Upgrade {
        /// Only say whether there is a newer version.
        #[arg(long)]
        check: bool,
        /// Turn the daily automatic update on.
        #[arg(long, conflicts_with = "off")]
        on: bool,
        /// Turn it off.
        #[arg(long)]
        off: bool,
        /// What the daily check runs on its own (silent).
        #[arg(long, hide = true)]
        background: bool,
        /// What the new binary runs after replacing the old one (the old version).
        #[arg(long, hide = true, value_name = "VERSION")]
        after: Option<String>,
    },
    /// What is worth reviewing: memories a newer one contradicts, stale status, broken paths, unused and too long.
    Review {
        /// From every project.
        #[arg(long)]
        all: bool,
        /// "It's fine as it is": stop warning about this memory (until it changes). Give its id.
        #[arg(long, value_name = "ID")]
        dismiss: Option<String>,
        /// With --dismiss: which warning (outdated, trade_duplicate, stale_status, code_drift, broken_path, unused, too_long, secret, due, review_date).
        #[arg(long, requires = "dismiss")]
        kind: Option<String>,
        /// With --dismiss on an outdated warning: the id of the newer memory.
        #[arg(long, requires = "dismiss")]
        other: Option<String>,
        /// Recompute in the background which memories' files changed a lot, and which trade memories say the same (Omoikane launches it itself).
        #[arg(long, hide = true)]
        refresh_drift: bool,
        /// Recompute in the background which trade memories say the same (Omoikane launches it itself).
        #[arg(long, hide = true)]
        refresh_trade_duplicates: bool,
    },
    /// A faithful copy: every memory with its history, forget marks, verdicts and the trash, one JSON line each (`omoi import` restores it exactly).
    #[command(group(clap::ArgGroup::new("espejo").args(["obsidian", "okf"])))]
    Export {
        /// From every project (default: this one and the global ones).
        #[arg(long, conflicts_with = "espejo")]
        all: bool,
        /// Encrypt every line with the key of its space's sync repository (AES-SIV; outside every space, the general one), to keep the copy anywhere.
        #[arg(long, conflicts_with = "espejo")]
        encrypt: bool,
        /// Instead: one Markdown note per memory in <vault>/omoikane/, for Obsidian's graph view (a mirror: rewritten on every export). Personal projects and the global ones by default.
        #[arg(long, value_name = "VAULT")]
        obsidian: Option<PathBuf>,
        /// Instead: an OKF v0.2 bundle (Open Knowledge Format) in this folder, one concept per memory (a mirror: rewritten on every export). Personal projects and the global ones by default.
        #[arg(long, value_name = "FOLDER")]
        okf: Option<PathBuf>,
        /// With --obsidian or --okf: only this space (a client), without the global memories. Give each space its own folder.
        #[arg(long, requires = "espejo")]
        space: Option<String>,
        /// With --obsidian: keep the vault up to date, exporting again when the database changes.
        #[arg(long, requires = "obsidian")]
        watch: bool,
        /// With --watch: seconds between checks.
        #[arg(long, requires = "watch", default_value_t = 60, value_parser = clap::value_parser!(u64).range(5..))]
        interval: u64,
    },
    /// Copy the database now to <its folder>/backups/omoikane-<date>.db (it also happens by itself once a day; OMOIKANE_BACKUPS copies are kept, 7 by default).
    Backup {
        /// Only list the copies there are.
        #[arg(long)]
        list: bool,
    },
    /// Browse, search, pin and forget memories in the terminal, and restore or delete them from the trash.
    Tui {
        /// Start with every project seen from here: its space (from the home folder, all of them). Default: this one and the global ones.
        #[arg(long)]
        all: bool,
    },
    /// Import an `omoi export` file (exactly: same ids, dates, history and verdicts; newest wins, so it is safe to repeat), an engram export, an OKF bundle (--okf) or what was edited in an Obsidian vault (--obsidian).
    #[command(group(clap::ArgGroup::new("carpeta_importada").args(["okf", "obsidian"])))]
    Import {
        /// The export file.
        #[arg(required_unless_present_any = ["okf", "obsidian"])]
        fichero: Option<PathBuf>,
        /// Instead: an OKF v0.2 bundle (a folder of Markdown concepts, like a repository's knowledge/). Each concept updates the same memory when imported again. Into this folder's project, or with --space into that space.
        #[arg(long, value_name = "FOLDER", conflicts_with = "fichero")]
        okf: Option<PathBuf>,
        /// Instead: bring back what was edited in a vault made by `omoi export --obsidian`. An edited note updates its memory (with history); a new note in a project's folder, with a `kind:`, becomes a new memory there. Then the vault is exported again.
        #[arg(long, value_name = "VAULT", conflicts_with_all = ["fichero", "okf"])]
        obsidian: Option<PathBuf>,
        /// With --obsidian: a note edited in Obsidian whose memory also changed here since the export (a conflict) keeps Obsidian's version (`obsidian`; Omoikane's stays in the history) or Omoikane's (`omoikane`; the note's change is lost). Without it, neither is touched.
        #[arg(long, requires = "obsidian", value_parser = ["obsidian", "omoikane"])]
        prefer: Option<String>,
        /// With --okf or --obsidian: into this space (a client), without a project. With --obsidian, the space the vault was exported from.
        #[arg(long, requires = "carpeta_importada")]
        space: Option<String>,
        /// With --okf: into the global memories (every project sees them). Needed to import from the home folder.
        #[arg(long, requires = "okf", conflicts_with = "space")]
        global: bool,
        /// Also keep the "verified by a person" that someone else put: in a bundle that wasn't exported from this database, or in a file whose verifications you didn't sign (someone else's export). Never from a coding agent's shell.
        #[arg(long)]
        trust_verified: bool,
        /// With --okf --space: import without asking even if it brings pinned, verified or personal memories into the client's space (its repository is shared).
        #[arg(long, requires = "space")]
        yes: bool,
        /// Import even what looks like a memory already saved.
        #[arg(short, long)]
        force: bool,
        /// Only say what would be imported.
        #[arg(long)]
        dry_run: bool,
    },
    /// The embedding model that enables search by meaning.
    Model {
        #[command(subcommand)]
        accion: Option<ModelAction>,
    },
    /// Rebuild the search indexes (words and vectors) from the memories.
    Reindex,
    /// Serve the memory to an AI agent over MCP (stdio). Agents launch it themselves.
    Mcp,
    /// Serve the same tools over HTTP on 127.0.0.1 (for scripts and apps that don't speak MCP), and a web
    /// view of every memory at http://127.0.0.1:7438/.
    Serve {
        /// The port.
        #[arg(long, default_value_t = omoikane::http::PUERTO)]
        port: u16,
        /// Open the web view in the browser (if a server is already running, just open it).
        #[arg(long)]
        open: bool,
        /// Open (with --open) or print the link to this memory: its panel, with the Verify button
        /// (`http://127.0.0.1:7438/#/memoria/<id>`). Its id or the beginning of it.
        #[arg(long, value_name = "ID")]
        memory: Option<String>,
    },
    /// Share memories between your computers (or with a team) through a private git repository of
    /// yours. Without an action, sync now.
    Sync {
        #[command(subcommand)]
        accion: Option<SyncAction>,
        /// Run in the background after a change (Omoikane launches it itself).
        #[arg(long, hide = true)]
        background: bool,
        /// Bring in what other computers pushed, if anything (Omoikane launches it itself, at most every
        /// 2 minutes while you work).
        #[arg(long, hide = true)]
        pull: bool,
    },
    /// Set up an agent to use Omoikane: claude, codex, gemini, cursor, opencode, qwen, kiro, copilot, windsurf, antigravity, kimi, pi, amp, kilo or grok.
    Setup {
        /// The agent: claude, codex, gemini, cursor, opencode, qwen, kiro, copilot, windsurf, antigravity, kimi, pi, amp, kilo or grok.
        agente: String,
        /// Don't ask before changing anything.
        #[arg(short, long)]
        yes: bool,
        /// Only show what would be done.
        #[arg(long)]
        dry_run: bool,
        /// Claude Code: don't turn on the automatic update (Omoikane's daily one and the plugin's).
        #[arg(long)]
        no_auto_update: bool,
    },
    /// For Claude Code hooks: reads the hook's JSON on stdin and prints the memories to inject.
    Recall {
        /// At session start (or after compacting): preferences and pinned memories.
        #[arg(long)]
        session_start: bool,
        /// When a session ends or compacts: note it in the session diary (`omoi diary`) and set aside what
        /// may be worth saving from the conversation (`transcript_path`), for the agent of the next
        /// session. Prints nothing.
        #[arg(long, conflicts_with = "session_start")]
        session_end: bool,
        /// Before a tool opens a file (the `PreToolUse` hook of Read, Edit and Write): the memories tied to
        /// that file by their paths, and its history in the session diary, once per session.
        #[arg(long, conflicts_with_all = ["session_start", "session_end"])]
        file_open: bool,
        /// The agent's hook format: claude (also Codex and Qwen), gemini, cursor, copilot, antigravity or plain.
        #[arg(long, value_enum, default_value = "claude")]
        format: recall::HookFormat,
    },
    /// Check SQLite, FTS5 and sqlite-vec, and show where the database is.
    Doctor,
    /// For clients that don't speak MCP (the Claude Code plugin): runs one of the MCP tools with
    /// its JSON arguments on stdin and prints its result. Without a name, lists the tools.
    #[command(hide = true)]
    Tool {
        /// The tool: remember, search, get, update, forget, history, context, recent or decisions.
        nombre: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProjectsAction {
    /// The projects with memories, latest activity first (the default).
    List,
    /// The project now lives in another folder (it moved without a git remote to recognise it), or goes to a space: its memories follow it.
    Move {
        /// The project: its old folder, or its name or part of it.
        proyecto: String,
        /// Its new folder.
        #[arg(required_unless_present = "space")]
        carpeta: Option<PathBuf>,
        /// Put it in this space.
        #[arg(long, value_name = "SPACE")]
        space: Option<String>,
        /// Merge it even if the folder is another space's project, or move it from one space to
        /// another, or put a paused project in a space (its memories change space).
        #[arg(long)]
        force: bool,
    },
    /// A paused project stays where it is here, and syncs again: when another computer says it is in
    /// another space (to accept that instead: `omoi projects move <project> --space <space> --force`),
    /// or when this folder joined a space's project only because that space's repository says so (it
    /// is that space's; if it is yours: `omoi space unassign <project> --force`).
    Keep {
        /// The project: its folder, or its name or part of it.
        proyecto: String,
    },
    /// Forget the projects without memories (a folder you work in again comes back by itself).
    Prune,
    /// Tell the review that A and B are different projects (or, without B, that A is a real project
    /// even if its folder looks temporary): it stops proposing to merge them. Only on this computer.
    Distinct {
        /// A project (folder or name).
        a: String,
        /// The other project (folder or name).
        b: Option<String>,
    },
    /// Merge project A into B: A's memories (with their ids, history and verdicts) and folders go to B.
    Merge {
        /// The project that disappears (folder or name).
        a: String,
        /// The project that stays (folder or name).
        b: String,
        /// Merge them even if they are in different spaces (the memories change space).
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum TradesAction {
    /// This project's trades and where they come from (the default); with --all, every project's.
    List {
        /// Every project, and how many trade memories each trade has.
        #[arg(long)]
        all: bool,
    },
    /// The project has these trades too, even if they aren't detected.
    Add {
        /// Trades: android, web, wordpress, seo-geo…
        #[arg(required = true, value_name = "TRADE")]
        oficios: Vec<String>,
        /// The project: its folder, or its name or part of it (by default, this folder's).
        #[arg(long, value_name = "PROJECT")]
        project: Option<String>,
    },
    /// The project doesn't have these trades, even if they are detected.
    Remove {
        /// Trades: android, web, wordpress, seo-geo…
        #[arg(required = true, value_name = "TRADE")]
        oficios: Vec<String>,
        /// The project: its folder, or its name or part of it (by default, this folder's).
        #[arg(long, value_name = "PROJECT")]
        project: Option<String>,
    },
    /// Exactly these trades (none, without any), and stop detecting.
    Set {
        /// Trades: android, web, wordpress, seo-geo…
        #[arg(value_name = "TRADE")]
        oficios: Vec<String>,
        /// The project: its folder, or its name or part of it (by default, this folder's).
        #[arg(long, value_name = "PROJECT")]
        project: Option<String>,
    },
    /// Forget the manual changes and detect them again from the files.
    Auto {
        /// The project: its folder, or its name or part of it (by default, this folder's).
        #[arg(long, value_name = "PROJECT")]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum SpaceAction {
    /// The spaces, with their projects (the default).
    List,
    /// Create a space.
    Add {
        /// Its name (a client, a product…).
        nombre: String,
    },
    /// Put a project in a space (by default, this folder's project).
    Assign {
        /// The space.
        espacio: String,
        /// The project: its folder, or its name or part of it.
        proyecto: Option<String>,
        /// Move it even if it is already in another space, or if it is paused (its memories go to that
        /// space's repository).
        #[arg(long)]
        force: bool,
    },
    /// Take a project out of its space (by default, this folder's project).
    Unassign {
        /// The project: its folder, or its name or part of it.
        proyecto: Option<String>,
        /// Do it: without a space, its memories go to the general (personal) repository.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum SyncAction {
    /// Set up a new shared repository (empty, or without memories yet): any git URL you can push to.
    Init {
        /// The repository: git@github.com:you/memory.git, https://…, a folder…
        #[arg(required_unless_present = "create")]
        url: Option<String>,
        /// Create it first as a private repository with this name, with the GitHub (`gh`) or GitLab
        /// (`glab`) command line if one is installed and signed in.
        #[arg(long, value_name = "NAME", conflicts_with = "url")]
        create: Option<String>,
        /// A repository (with its own key) for one space only, besides the general one: it carries that
        /// space's projects and its own memories, never global ones (the general one never carries a
        /// space's memories).
        #[arg(long, value_name = "NAME")]
        space: Option<String>,
        #[command(flatten)]
        eleccion: SyncOptions,
    },
    /// Join a repository another computer already syncs with, with the code from `omoi sync code`.
    Join {
        /// The code printed by `omoi sync code` (`-` reads it from standard input, so it doesn't stay in
        /// the shell history; without it, the codes in OMOIKANE_SYNC_CODE).
        codigo: Option<String>,
        /// Reach the same repository by another URL (for example SSH here and HTTPS on the other computer).
        #[arg(long)]
        url: Option<String>,
        /// This computer belongs to another person (a teammate): their projects without a git remote
        /// stay separate from the inviter's. By default a joining computer is the inviter's own.
        #[arg(long)]
        another_person: bool,
        /// The space this repository is for (by default, the one in the code; none: the general one).
        #[arg(long, value_name = "NAME")]
        space: Option<String>,
        #[command(flatten)]
        eleccion: SyncOptions,
    },
    /// Print the code to join from another computer. It carries the current key: share it privately.
    Code {
        /// The code of this space's repository.
        #[arg(long, value_name = "NAME")]
        space: Option<String>,
    },
    /// How sync is set up and how the last one went (every repository).
    Status,
    /// Change the repository's key: re-encrypt this computer's memories with a new one and print the
    /// new code. Computers with only the old key stop reading what is written from now on.
    Rotate {
        /// The key of this space's repository.
        #[arg(long, value_name = "NAME")]
        space: Option<String>,
        /// Don't ask.
        #[arg(short, long)]
        yes: bool,
    },
    /// Bring back a memory another computer forgot (without an id, list them). Its history comes back too.
    Restore {
        /// The memory id (its first characters are enough).
        id: Option<String>,
    },
    /// Stop syncing on this computer (its memories stay).
    Off {
        /// Don't ask.
        #[arg(short, long)]
        yes: bool,
        /// Only this space's repository.
        #[arg(long, value_name = "NAME")]
        space: Option<String>,
    },
    /// Forget the verification keys noted from earlier general repositories. For a computer that joined a teammate's general repository with 1.8.5 (without --another-person noted): their verifications kept counting as yours. Yours signed only with a forgotten key stop counting here until you verify them again.
    ForgetSealKeys {
        /// Don't ask.
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Args)]
struct SyncOptions {
    /// Don't ask: use these options and the defaults.
    #[arg(short, long)]
    yes: bool,
    /// Also share preferences (by default they stay on this computer).
    #[arg(long)]
    share_preferences: bool,
    /// Don't share global memories.
    #[arg(long)]
    no_global: bool,
    /// Share only this project (repeatable; by default, every project).
    #[arg(long, value_name = "DIR")]
    project: Vec<PathBuf>,
    /// Don't write an author on each memory.
    #[arg(long, conflicts_with = "author")]
    no_author: bool,
    /// The author written on each memory (by default, git's user.name).
    #[arg(long)]
    author: Option<String>,
}

#[derive(Subcommand)]
enum DocsAction {
    /// The documentation folders of this project, with what is indexed (the default).
    List,
    /// Add a folder of Markdown files and index it now. Nothing is copied into memory, and the files are never written.
    Add {
        /// The folder (docs/, a wiki, an Obsidian vault).
        carpeta: PathBuf,
    },
    /// Stop using a folder (its index goes too).
    Remove {
        /// The folder, as `omoi docs list` shows it.
        carpeta: String,
    },
    /// Bring the index up to date now (it also happens, on the side, when each session starts).
    Refresh,
    /// Which section recall would bring for this text, without noting it as given.
    Test {
        /// The prompt to try.
        texto: String,
    },
}

#[derive(Subcommand)]
enum ModelAction {
    /// Which model is installed and in use (the default).
    Status,
    /// Download the model (about 150 MB, once) and compile it.
    Install {
        /// Take it from a folder with the int8 model instead of downloading it.
        #[arg(long, value_name = "DIR")]
        from: Option<PathBuf>,
        /// Install the meaning model (EmbeddingGemma, about 450 MB) instead of the light one. With
        /// --from, a folder with it in the compact format or as sentence-transformers leaves it.
        #[arg(long)]
        semantic: bool,
    },
    /// Delete installed models that are not in use.
    Clean,
    /// Whether the meaning model (EmbeddingGemma) is in use and why. Unless you choose, it turns on by
    /// itself once the memory is large (1,000 memories): below that, words alone are as good and faster.
    Semantic {
        /// Use it even with a small memory (downloads it in the background if it is missing).
        #[arg(long, conflicts_with_all = ["off", "auto"])]
        on: bool,
        /// Never use it, whatever the size, and close its service.
        #[arg(long, conflicts_with = "auto")]
        off: bool,
        /// Let the size of the memory decide again (the default).
        #[arg(long)]
        auto: bool,
        /// RAM the indexing may use, in MB (0: back to the default). Bigger indexes faster in batches;
        /// `OMOIKANE_SEMANTIC_RAM` overrides it.
        #[arg(long, value_name = "MB")]
        ram: Option<u64>,
    },
    /// The service that keeps the meaning model loaded (Omoikane starts it by itself).
    #[command(hide = true)]
    ServeSemantic,
    /// Compute now, in this process, the meaning vectors this database is missing (the service does it
    /// by itself in the background; this is for scripts and benchmarks that need to wait for it).
    #[command(hide = true)]
    IndexSemantic,
}

fn main() -> Result<()> {
    // Rust ignores SIGPIPE, so `println!` blows up if the output is closed (`omoi … | head`).
    // The norm on Unix is to exit without a word.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let mut cli = Cli::from_arg_matches(&in_spanish(Cli::command()).get_matches()).unwrap_or_else(|e| e.exit());
    // The database with an absolute path from the start: a relative one (`--db b/omoikane.db`) is read from
    // other folders (git clones the sync from the database's folder) and would not be found.
    if let Some(d) = cli.comun.db.take() {
        cli.comun.db = Some(std::path::absolute(&d).unwrap_or(d));
    }
    // No user folder (a container) and no variables saying where: a clear error, not a new database
    // in the folder it runs in.
    // `setup` and `upgrade` don't open the database: they don't need it.
    let no_db = matches!(cli.orden, Commands::Setup { .. } | Commands::Upgrade { .. });
    if let Some(e) = omoikane::entorno::casa_mala() {
        bail!(e);
    }
    // The database and the models, only on this computer: with `\\server\share` (or `//host/x`) in `--db`,
    // `OMOIKANE_DB` or `OMOIKANE_MODELS`, every command would open an SMB connection there.
    if !no_db
        && (cli.comun.db.is_some() || omoikane::entorno::hay_donde_guardar())
        && let Some(e) = omoikane::entorno::ruta_mala(&tr!("the database", "la base"), &cli.comun.db.clone().unwrap_or_else(db::ruta_por_defecto))
            .or_else(|| omoikane::entorno::ruta_mala("OMOIKANE_MODELS", &omoikane::modelos::carpeta()))
    {
        bail!(e);
    }
    if !no_db && cli.comun.db.is_none() && !omoikane::entorno::hay_donde_guardar() {
        bail!(omoikane::entorno::sin_casa());
    }
    let c = &cli.comun;
    if !matches!(cli.orden, Commands::Upgrade { .. }) {
        omoikane::actualizar::quizas_en_segundo_plano();
    }
    let writes = matches!(cli.orden, Commands::Remember { .. } | Commands::Tags { merge: Some(_), .. } | Commands::Update(..) | Commands::Verify(..) | Commands::Forget { .. } | Commands::Restore { .. } | Commands::Import { .. } | Commands::Tui { .. });
    // Remember, update, forget and restore know which memory they touch and bring the review up to date
    // with only that one (`revisar::refresh_for`); an import or whatever is done in the view, all of it.
    let whole_review = matches!(cli.orden, Commands::Import { .. } | Commands::Tui { .. });
    let r = run(c, cli.orden);
    if writes && r.is_ok() {
        // Whatever needs reviewing is brought up to date for recall, which only reads (see `revisar`).
        if whole_review && let Ok(db) = open_db(c) {
            let _ = omoikane::revisar::refresh(&db);
        }
        omoikane::sync::programar(&db_path(c));
    }
    trace_wait();
    r
}

/// With `OMOIKANE_TRAZA`, how long this process waited for another to release the database (the
/// concurrency test adds it up per operation).
fn trace_wait() {
    if std::env::var_os("OMOIKANE_TRAZA").is_some_and(|v| !v.is_empty()) {
        eprintln!("TRAZA espera_base {}", serde_json::json!({ "ms": db::esperado().as_secs_f64() * 1000.0 }));
    }
}

fn db_path(c: &Common) -> PathBuf {
    c.db.clone().unwrap_or_else(db::ruta_por_defecto)
}

/// The content of `remember`: the argument or standard input. With `--data` and no content, the
/// memory is mostly its data: standard input is not awaited and the content is the title.
fn content_of(content: Option<String>, title: &str, con_datos: bool) -> Result<String> {
    if con_datos && content.is_none() {
        return Ok(title.to_string());
    }
    text_or_stdin(content)
}

/// `omoi export --okf`: apart from `run` so as not to enlarge its stack frame.
fn export_okf(c: &Common, folder: &Path, space: Option<String>) -> Result<()> {
    let db = open_db(c)?;
    let what = match space {
        Some(s) => omoikane::obsidian::Que::Espacio(proyecto::buscar_espacio(&db, &s)?),
        None => omoikane::obsidian::Que::Personal,
    };
    let n = omoikane::okf::exportar(&db, folder, what)?;
    if c.json {
        return json(&n);
    }
    eprintln!(
        "{}",
        tr!(
            "{} memories in the OKF bundle {}: {} new, {} changed, {} unchanged, {} deleted",
            "{} memorias en el bundle OKF {}: {} nuevas, {} cambiadas, {} iguales, {} borradas",
            n.memories,
            folder.display(),
            n.created,
            n.updated,
            n.unchanged,
            n.deleted
        )
    );
    Ok(())
}

/// `omoi import --obsidian`: apart from `run` so as not to enlarge its stack frame.
fn import_obsidian(c: &Common, vault: &Path, space: Option<String>, prefer: Option<&str>, dry_run: bool) -> Result<()> {
    use omoikane::obsidian::{Preferir, Que};
    let (mut db, model) = open_with_model(c)?;
    let what = match space {
        Some(s) => Que::Espacio(proyecto::buscar_espacio(&db, &s)?),
        None => Que::Personal,
    };
    let prefer_mode = match prefer {
        Some("obsidian") => Preferir::Obsidian,
        Some("omoikane") => Preferir::Omoikane,
        _ => Preferir::Nada,
    };
    let r = omoikane::obsidian::importar(&mut db, model.as_ref(), vault, what, prefer_mode, dry_run)?;
    if !dry_run && r.updated + r.created > 0 {
        omoikane::sync::programar(&db_path(c));
    }
    if c.json {
        let mut v = serde_json::to_value(&r)?;
        v["dry_run"] = serde_json::json!(dry_run);
        return json(&v);
    }
    for a in &r.notices {
        eprintln!("omoikane: {a}");
    }
    let listing = |v: &[String]| {
        let mut s: String = v.iter().take(10).map(|x| format!("\n  {x}")).collect();
        if v.len() > 10 {
            s += &format!("\n  … (+{})", v.len() - 10);
        }
        s
    };
    if !r.conflicts.is_empty() {
        eprintln!(
            "{}{}",
            tr!(
                "omoikane: {} notes were edited in Obsidian and their memory also changed here after the export: neither was touched (the export won't overwrite them). Compare them with `omoi history <id>`, then import again with --prefer obsidian or --prefer omoikane:",
                "omoikane: {} notas se editaron en Obsidian y su memoria también cambió aquí después de exportarlas: no se ha tocado ninguna de las dos (la exportación no las pisa). Compáralas con `omoi history <id>` y vuelve a importar con --prefer obsidian o --prefer omoikane:",
                r.conflicts.len()
            ),
            listing(&r.conflicts)
        );
    }
    if !r.foreign.is_empty() {
        eprintln!(
            "{}{}",
            tr!(
                "omoikane: {} notes say which memory they are but weren't exported from this database for this vault (another database, a version before 1.9, or those fields were edited), and their text isn't their memory's: they don't touch any memory, and the export after the import left them as they are. If you edited one, bring the change over with `omoi update <id>`, or remove its `omoikane:` line to bring it in as a new memory; then delete the note, and the next export writes this database's copy:",
                "omoikane: {} notas dicen de qué memoria son pero no se exportaron desde esta base para este vault (otra base, una versión anterior a la 1.9, o se tocaron esos campos), y su texto no es el de su memoria: no tocan ninguna memoria, y la exportación de después de importar las ha dejado como estaban. Si editaste alguna, lleva el cambio con `omoi update <id>`, o quítale la línea `omoikane:` para traerla como memoria nueva; luego borra la nota y la próxima exportación escribe la copia de esta base:",
                r.foreign.len()
            ),
            listing(&r.foreign)
        );
    }
    if !r.gone.is_empty() {
        eprintln!(
            "{}{}",
            tr!(
                "omoikane: {} edited notes are of memories that are no longer in this vault (forgotten, or moved elsewhere): left as they are, delete them when you're done:",
                "omoikane: {} notas editadas son de memorias que ya no están en este vault (olvidadas, o llevadas a otro sitio): se quedan como están, bórralas cuando acabes con ellas:",
                r.gone.len()
            ),
            listing(&r.gone)
        );
    }
    for s in r.skipped.iter().chain(&r.failed) {
        eprintln!("omoikane: {}: {}", s.note, s.reason);
    }
    if !r.redacted.is_empty() {
        eprintln!("{}{}", tr!("omoikane: something that looked like a secret was hidden in:", "omoikane: se tapó algo que parecía un secreto en:"), listing(&r.redacted));
    }
    if r.unverified > 0 {
        eprintln!(
            "{}",
            tr!(
                "omoikane: {} memories verified by a person changed their text: they are no longer verified (`omoi verify` to verify them again)",
                "omoikane: {} memorias verificadas por una persona han cambiado de texto: ya no están verificadas (`omoi verify` para volver a verificarlas)",
                r.unverified
            )
        );
    }
    let resolved = if r.conflicts_applied + r.conflicts_discarded > 0 {
        tr!("; conflicts: {} with Obsidian's version, {} with Omoikane's", "; conflictos: {} con lo de Obsidian, {} con lo de Omoikane", r.conflicts_applied, r.conflicts_discarded)
    } else {
        String::new()
    };
    let skipped = r.skipped.len() + r.foreign.len() + r.gone.len();
    if dry_run {
        println!(
            "{}{resolved}",
            tr!(
                "{} notes in {}: {} memories would be updated and {} created, {} unchanged, {} in conflict, {} skipped, {} failed",
                "{} notas en {}: se pondrían al día {} memorias y se crearían {}, {} iguales, {} en conflicto, {} saltadas, {} fallidas",
                r.notes,
                vault.display(),
                r.updated,
                r.created,
                r.unchanged,
                r.conflicts.len(),
                skipped,
                r.failed.len()
            )
        );
    } else {
        println!(
            "{}{resolved}",
            tr!(
                "imported {} notes from {}: {} memories updated, {} new, {} unchanged, {} in conflict, {} skipped, {} failed",
                "importadas {} notas de {}: {} memorias puestas al día, {} nuevas, {} iguales, {} en conflicto, {} saltadas, {} fallidas",
                r.notes,
                vault.display(),
                r.updated,
                r.created,
                r.unchanged,
                r.conflicts.len(),
                skipped,
                r.failed.len()
            )
        );
    }
    Ok(())
}

/// `omoi import --okf`: apart from `run` so as not to enlarge its stack frame.
fn import_okf(c: &Common, folder: &Path, space: Option<String>, global: bool, trust: bool, yes: bool, dry_run: bool) -> Result<()> {
    // From the home folder, everything would go global (any project sees it): only if asked for.
    let current = project_of(c)?;
    anyhow::ensure!(
        space.is_some() || global || !proyecto::es_general(&current),
        tr!(
            "this is your home folder, not a project: importing here would make every concept global (seen from every project). Run it in the project's folder (or give --dir <project>), use --space <name> for a client, or --global if you do want them global",
            "esta es tu carpeta personal, no un proyecto: importar aquí haría globales todos los conceptos (los vería cualquier proyecto). Lánzalo en la carpeta del proyecto (o con --dir <proyecto>), usa --space <nombre> para un cliente, o --global si de verdad las quieres globales"
        )
    );
    let bundle = omoikane::okf::leer(folder)?;
    for a in &bundle.avisos {
        eprintln!("omoikane: {a}");
    }
    if let Some(v) = bundle.version.as_deref().filter(|v| !v.starts_with("0.")) {
        eprintln!("{}", tr!("omoikane: the bundle says OKF {v}; this version reads 0.2: importing what it understands", "omoikane: el bundle dice OKF {v}; esta versión lee la 0.2: se importa lo que entiende"));
    }
    let (mut db, model) = open_with_model(c)?;
    let target = match space {
        Some(s) => omoikane::okf::Destino::Espacio(proyecto::buscar_espacio(&db, &s)?),
        None if global => omoikane::okf::Destino::Proyecto(String::new()),
        None => omoikane::okf::Destino::Proyecto(current),
    };
    // Importing is the person's call: a person's `verified` sets the seal, except inside an agent's
    // shell (like `omoi verify`), and only what was exported from this database unless
    // --trust-verified.
    let agent = omoikane::verificado::shell_de_agente();
    let op = omoikane::okf::Opciones { en_seco: dry_run, verificar: agent.is_none(), confiar_verificadas: trust && agent.is_none() };
    // Into a client's space (other people share its repository): pinned, verified and personal
    // (Omoikane memories from outside that space) are warned about, and without --yes it asks (with
    // nobody there, it stops).
    if let omoikane::okf::Destino::Espacio(e) = &target {
        let preview = omoikane::okf::importar_con(&mut db, model.as_ref(), &bundle, &target, omoikane::okf::Opciones { en_seco: true, ..op })?;
        let key = proyecto::clave_espacio(&db, Some(*e))?;
        let outside = omoikane::okf::de_fuera_del_espacio(&bundle, &key);
        if preview.pinned + preview.verified + outside > 0 {
            let notice = tr!(
                "this goes into the space «{key}», whose sync repository other people share: {} pinned, {} verified by a person and {} memories from outside that space (personal or another client's) would come in",
                "esto va al espacio «{key}», cuyo repositorio de sincronización comparten otras personas: entrarían {} fijadas, {} verificadas por una persona y {} memorias de fuera de ese espacio (personales o de otro cliente)",
                preview.pinned,
                preview.verified,
                outside
            );
            eprintln!("omoikane: {notice}");
            if !dry_run && !yes {
                anyhow::ensure!(
                    omoikane::entorno::puede_preguntar(),
                    tr!("nothing was imported: add --yes if that is what you want", "no se ha importado nada: añade --yes si es lo que quieres")
                );
                if !ask_yes_no(&tr!("Import them into the space?", "¿Importarlas al espacio?"), false)? {
                    bail!(tr!("nothing was imported", "no se ha importado nada"));
                }
            }
        }
    }
    let r = omoikane::okf::importar_con(&mut db, model.as_ref(), &bundle, &target, op)?;
    if r.untrusted_verified > 0 {
        eprintln!(
            "{}",
            tr!(
                "omoikane: {} concepts say a person verified them, but the bundle wasn't exported from this database: they come in unverified. If you trust whoever made it, import again with --trust-verified",
                "omoikane: {} conceptos dicen que una persona los verificó, pero el bundle no se exportó de esta base: entran sin verificar. Si te fías de quien lo hizo, vuelve a importar con --trust-verified",
                r.untrusted_verified
            )
        );
    }
    if r.stale_verified > 0 {
        eprintln!(
            "{}",
            tr!(
                "omoikane: {} concepts carry a verification that no longer holds (their text changed since, you unverified them later, or the date is in the future): they come in unverified",
                "omoikane: {} conceptos traen una verificación que ya no vale (su texto ha cambiado desde entonces, la quitaste después o la fecha es del futuro): entran sin verificar",
                r.stale_verified
            )
        );
    }
    if r.ignored_global + r.ignored_pinned > 0 {
        eprintln!(
            "{}",
            tr!(
                "omoikane: the bundle wasn't exported from this database: {} concepts asked to be global and go to {} instead; {} asked to be pinned and this import doesn't pin them",
                "omoikane: el bundle no se exportó de esta base: {} conceptos pedían ser globales y van a {} en su lugar; {} pedían estar fijados y esta importación no los fija",
                r.ignored_global,
                match &target {
                    omoikane::okf::Destino::Proyecto(p) if !p.is_empty() => p.clone(),
                    _ => tr!("the destination", "el destino").to_string(),
                },
                r.ignored_pinned
            )
        );
    }
    if r.skipped_forgotten > 0 {
        eprintln!(
            "{}",
            tr!(
                "omoikane: {} concepts are memories you forgot here: skipped (to bring one back, `omoi restore <id>`)",
                "omoikane: {} conceptos son memorias que olvidaste aquí: saltados (para recuperar una, `omoi restore <id>`)",
                r.skipped_forgotten
            )
        );
    }
    if r.skipped_older > 0 {
        eprintln!(
            "{}",
            tr!(
                "omoikane: {} concepts are older than their memory here (exported before it changed): left as they are here, with no verification or verdicts from the bundle",
                "omoikane: {} conceptos son más viejos que su memoria de aquí (exportados antes de que cambiara): se quedan como están aquí, sin la verificación ni los veredictos del bundle",
                r.skipped_older
            )
        );
    }
    if let Some(var) = agent.filter(|_| r.not_verified > 0) {
        eprintln!(
            "{}",
            tr!(
                "omoikane: {} concepts say a person verified them, but this looks like a coding agent's shell ({var} is set): they come in unverified. Import again from your own terminal to keep the verification",
                "omoikane: {} conceptos dicen que una persona los verificó, pero esto parece la shell de un agente ({var} está puesta): entran sin verificar. Vuelve a importar desde tu terminal para conservarla",
                r.not_verified
            )
        );
    }
    if c.json {
        let mut v = serde_json::to_value(&r)?;
        v["dry_run"] = serde_json::json!(dry_run);
        return json(&v);
    }
    if dry_run {
        println!(
            "{}",
            tr!(
                "{} concepts in {}: {} new and {} already here (they would be updated); the bundle checks out",
                "{} conceptos en {}: {} nuevos y {} que ya están (se pondrían al día); el bundle está bien",
                r.concepts,
                bundle.raiz.display(),
                r.created,
                r.updated
            )
        );
    } else {
        println!(
            "{}",
            tr!(
                "imported {} concepts: {} new, {} updated, {} unchanged, {} failed; {} verdicts",
                "importados {} conceptos: {} nuevas, {} actualizadas, {} iguales, {} fallidas; {} veredictos",
                r.concepts,
                r.created,
                r.updated,
                r.unchanged,
                r.failed,
                r.verdicts
            )
        );
    }
    if r.global + r.pinned > 0 {
        println!(
            "{}",
            if dry_run {
                tr!("{} would come in as global (seen from every project) and {} pinned", "{} entrarían como globales (las ve cualquier proyecto) y {} fijadas", r.global, r.pinned)
            } else {
                tr!("{} came in as global (seen from every project) and {} pinned", "{} entraron como globales (las ve cualquier proyecto) y {} fijadas", r.global, r.pinned)
            }
        );
    }
    if r.verified > 0 {
        println!(
            "{}",
            if dry_run {
                tr!("{} would be marked as verified by a person (as the bundle says)", "{} se marcarían como verificadas por una persona (como dice el bundle)", r.verified)
            } else {
                tr!("{} marked as verified by a person (as the bundle says)", "{} marcadas como verificadas por una persona (como dice el bundle)", r.verified)
            }
        );
    }
    if !r.redacted.is_empty() {
        eprintln!("{}", tr!("omoikane: something that looked like a secret was masked in: {}", "omoikane: se tapó algo que parecía un secreto en: {}", r.redacted.join("; ")));
    }
    Ok(())
}

/// `remember --key`: the one with that key here, created or updated, without looking for duplicates. Apart from
/// `run` so as not to enlarge its stack frame (in debug, every branch adds up).
fn remember_by_key(
    c: &Common,
    db: &mut rusqlite::Connection,
    key: &str,
    new_memory: NewMemory,
    data: Option<serde_json::Value>,
    model: Option<&omoikane::embeber::Modelo>,
    before: &omoikane::revisar::Before,
) -> Result<()> {
    let r = memoria::save_by_key(db, key, new_memory, data, model)?;
    let _ = omoikane::revisar::refresh_for(db, before, std::slice::from_ref(&r.memory.id));
    warn_redacted(&r.memory);
    warn_masked_data(&r.redacted);
    if c.json {
        // The memory with its data (`data`), if any, like `get --json`.
        let mut v = memoria::with_data(db, vec![r.memory])?.remove(0);
        v["created"] = serde_json::json!(r.created);
        if !r.similar_keys.is_empty() {
            v["similar_keys"] = serde_json::json!(r.similar_keys);
        }
        return json(&v);
    }
    let id = short_id(&r.memory.id);
    println!("{}", if r.created { tr!("saved {id}", "guardada {id}") } else { tr!("updated {id}", "actualizada {id}") });
    if !r.similar_keys.is_empty() {
        println!("{}", tr!("  new key, but these keys already exist here: {}", "  clave nueva, pero aquí ya hay estas parecidas: {}", r.similar_keys.join(", ")));
    }
    Ok(())
}

/// `get --key --resolve`: every layer of the key for this project, without merging them.
fn print_key_layers(c: &Common, db: &rusqlite::Connection, key: &str) -> Result<()> {
    let layers = memoria::key_layers(db, &Scope::from_project(db, &project_of(c)?)?, key)?;
    if c.json {
        return json(&layers);
    }
    if layers.is_empty() {
        println!("{}", tr!("no memory with key «{}» here", "aquí no hay ninguna memoria con la clave «{}»", key.trim()));
    }
    for layer in &layers {
        println!("[{}] {}  {}", layer.scope, short_id(&layer.id), layer.title);
        if let Some(d) = &layer.data {
            println!("  data: {d}");
        }
    }
    Ok(())
}

fn run(c: &Common, command: Commands) -> Result<()> {
    match command {
        Commands::Doctor => doctor(c),
        Commands::Model { accion: action } => model(c, action.unwrap_or(ModelAction::Status)),
        Commands::Mcp => omoikane::mcp::servir(c.db.clone().unwrap_or_else(db::ruta_por_defecto), c.dir.clone()),
        Commands::Serve { port, open, memory } => {
            // The whole id, and that this folder's view shows it, before starting anything.
            let memory = match memory {
                Some(id) => {
                    let db = open_db(c)?;
                    Some(omoikane::http::memoria_del_enlace(&db, &project_of(c)?, &id)?)
                }
                None => None,
            };
            omoikane::http::servir(c.db.clone().unwrap_or_else(db::ruta_por_defecto), c.dir.clone(), port, open, memory)
        }
        Commands::Tool { nombre: name } => {
            let mut args = String::new();
            if name.is_some() {
                std::io::stdin().read_to_string(&mut args)?;
            }
            let db = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
            // The caller says who it is (the Claude Code mod sets mcp:claude-code).
            let origin = std::env::var("OMOIKANE_SOURCE").ok().filter(|o| !o.trim().is_empty()).unwrap_or_else(|| "mcp".into());
            let (text, is_error) = omoikane::mcp::herramienta(db, c.dir.clone(), name.as_deref(), &args, &origin)?;
            println!("{text}");
            // Exit 1 if the tool answered with isError: the client marks it as an error.
            trace_wait();
            std::process::exit(if is_error { 1 } else { 0 })
        }
        Commands::Setup { agente: agent, yes, dry_run, no_auto_update } => setup(&agent, yes, dry_run, no_auto_update),
        Commands::Upgrade { check, on, off, background, after } => upgrade(check, on, off, background, after),
        Commands::Sync { accion: action, background, pull } => sync(c, action, background, pull),
        Commands::Recall { session_end: true, .. } => recall_on_session_end(c),
        Commands::Recall { session_start, file_open, format, .. } => {
            // A hook must never get in the way: if something fails, it says so on stderr and injects nothing.
            if let Err(e) = recall(c, session_start, file_open, format) {
                eprintln!("omoikane: {e:#}");
            }
            Ok(())
        }
        Commands::Reindex => {
            let mut db = open_db(c)?;
            let n = memoria::reindex_words(&mut db)?;
            let v = match modelos::abrir_actual()? {
                Some(m) => {
                    db.execute("DELETE FROM meta WHERE k = 'modelo'", [])?;
                    vectores::sincronizar(&mut db, &m)?
                }
                None => 0,
            };
            println!("{}", tr!("reindexed: {n} memories, {v} vectors", "reindexado: {n} memorias, {v} vectores"));
            Ok(())
        }
        Commands::Remember { kind, title, content, paths, tags, pinned, global, space, trades, para: audience, source, force, allow_leaks, key, datos: NewData { data, evidence, review_reason } } => {
            let content = content_of(content, &title, data.is_some())?;
            let evidence = omoikane::evidencia::clean(evidence.iter().map(|e| omoikane::evidencia::from_cli(e)).collect::<Result<Vec<_>>>()?)?;
            let data = data.as_deref().map(memoria::data_from_text).transpose()?;
            // Trade memory: `--trade`, or a `trade:` tag set by hand.
            let (trade_names, tags) = requested_trades(&trades, tags)?;
            let is_trade = !trade_names.is_empty();
            anyhow::ensure!(
                !(is_trade && space.is_some()),
                tr!("a trade memory belongs to no project and no space: drop --space", "una memoria de oficio no es de ningún proyecto ni espacio: quita --space")
            );
            let global = global || is_trade;
            let audience = audience.as_deref().map(omoikane::oficios::para).transpose()?.flatten();
            let tags = if is_trade { omoikane::oficios::etiquetas_de_oficio(tags, &trade_names, audience.as_deref()) } else { tags };
            // Without --dir, from a folder that is not a project (home) the memory goes to the repository
            // its paths point to, or it is global.
            let current = project_of(c)?;
            // Preferences belong to the person: global, unless the space or the folder (--dir) is given.
            let global = global || (kind == Kind::Preference && space.is_none() && c.dir.is_none());
            let (project, paths) =
                if global { (String::new(), paths) } else { proyecto::para_guardar(&current, &paths, &format!("{title} {content}")) };
            // From a general folder everything is visible: that is where duplicates are looked for.
            let search_in = if !project.is_empty() && proyecto::es_general(&current) { current.clone() } else { project.clone() };
            let (mut db, model) = open_with_model(c)?;
            // Everything it will carry: text, paths, tags, key and data (`--force` doesn't skip it).
            if is_trade {
                let fields = omoikane::oficios::campos_para_fugas(&[("title", &title), ("content", &content), ("paths", &paths.join(" "))], &tags, key.as_deref(), data.as_ref());
                warn_leaks(c, &omoikane::oficios::bloquean(omoikane::oficios::fugas_en(&db, &fields)?, allow_leaks))?;
            }
            if !force
                && key.is_none()
                && let Some(model) = &model
                && let Some((existing, sim)) = if is_trade {
                    // Among those of its trade, which can't be seen from the global scope.
                    vectores::duplicada_de_oficio(&db, model, &trade_names, &title, &content, None)?
                } else {
                    vectores::duplicada(&db, model, &search_in, &title, &content)?
                }
            {
                if c.json {
                    #[derive(serde::Serialize)]
                    struct Similar<'a> {
                        duplicate_of: &'a Memory,
                        similarity: f32,
                    }
                    json(&Similar { duplicate_of: &existing, similarity: sim })?;
                } else {
                    let id = short_id(&existing.id);
                    println!(
                        "{}",
                        tr!(
                            "not saved: it looks like {id} «{}» (similarity {sim:.2})\n  change that one with `omoi update {id}`, or save anyway with --force",
                            "no guardada: se parece a {id} «{}» (similitud {sim:.2})\n  cambia esa con `omoi update {id}`, o guárdala igualmente con --force",
                            existing.title
                        )
                    );
                }
                // Code 2: not an error, but the caller has to know it wasn't saved.
                std::process::exit(2);
            }
            let space_id = match space.as_deref().map(str::trim) {
                None => None,
                Some("") => Some(Scope::from_project(&db, &current)?.space().with_context(|| {
                    tr!("this project is not in a space: give its name (--space <name>), or `omoi space assign <space>`", "este proyecto no está en ningún espacio: di cuál (--space <nombre>), o `omoi space assign <espacio>`")
                })?),
                Some(n) => Some(proyecto::buscar_espacio(&db, n)?),
            };
            let before = omoikane::revisar::before(&db)?;
            // Where it comes from: the session and the commit (see `procedencia`); trade ones, nothing of the project;
            // global ones and a space's, only the session (this folder's branch may be a client's).
            let provenance = if is_trade {
                None
            } else if project.is_empty() || space_id.is_some() {
                omoikane::procedencia::session_only(&db, &current)
            } else {
                omoikane::procedencia::current(&db, &project)
            };
            // A variant of a tag that already exists where it will go (`ideas` with `idea`), becomes that one.
            let tags = if is_trade {
                tags
            } else {
                let (tags, changes) = omoikane::etiquetas::normalize_tags(&db, &omoikane::etiquetas::scope_for(&db, &project, space_id)?, tags)?;
                for x in &changes {
                    eprintln!("{}", tr!("  tag «{}» → «{}» (it already existed)", "  etiqueta «{}» → «{}» (ya existía)", x.from, x.to));
                }
                tags
            };
            let evidence = if is_trade { Vec::new() } else { evidence };
            if review_reason.is_some() && omoikane::revisar::review_date_of(&tags).is_none() {
                anyhow::bail!(tr!("--review-reason goes with a review date: add --tag review:YYYY-MM-DD too", "--review-reason va con una fecha de revisión: añade también --tag review:AAAA-MM-DD"));
            }
            let review_reason = review_reason.as_deref().and_then(memoria::clean_review_reason);
            let new_memory = NewMemory { project, space_id, kind: Some(kind), title, content, paths, tags, pinned, source, provenance, evidence, review_reason, ..Default::default() };
            if let Some(k) = key {
                return remember_by_key(c, &mut db, &k, new_memory, data, model.as_ref(), &before);
            }
            let m = memoria::save(&mut db, new_memory, model.as_ref())?;
            let _ = omoikane::revisar::refresh_for(&db, &before, std::slice::from_ref(&m.id));
            warn_redacted(&m);
            if let Some(d) = &data {
                warn_masked_data(&memoria::set_data(&mut db, &m.id, Some(d.clone()))?);
            }
            let rel = vectores::relacionadas(&db, &search_in, &m.title, &m.id, 3)?;
            if c.json {
                #[derive(serde::Serialize)]
                struct Saved<'a> {
                    #[serde(flatten)]
                    memoria: &'a Memory,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    data: Option<&'a serde_json::Value>,
                    #[serde(skip_serializing_if = "<[Memory]>::is_empty")]
                    related: &'a [Memory],
                }
                return json(&Saved { memoria: &m, data: data.as_ref(), related: &rel });
            }
            println!("{}", tr!("saved {}", "guardada {}", short_id(&m.id)));
            for r in &rel {
                println!("{}", tr!("  related: {} «{}»", "  relacionada: {} «{}»", short_id(&r.id), r.title));
            }
            if m.content.chars().count() > recall::RECALL_LEN {
                println!(
                    "{}",
                    tr!(
                        "  long memory: only its first {} characters count for recall; consider splitting it",
                        "  memoria larga: para el recall solo cuentan sus primeros {} caracteres; mejor partirla",
                        recall::RECALL_LEN
                    )
                );
            }
            Ok(())
        }
        Commands::Search { query, kind, tags, limit, all, project, space } => {
            let (mut db, model) = open_with_model(c)?;
            let scope = requested_scope(c, &db, all, project.as_deref(), space.as_deref())?;
            let all = !matches!(scope, Scope::Project { .. });
            let o = SearchOptions { scope, kind, tags, limit };
            let res = buscar::search(&mut db, &query.join(" "), &o, model.as_ref())?;
            if c.json {
                return json(&res);
            }
            if res.is_empty() {
                println!("{}", tr!("nothing found", "nada"));
            }
            for r in &res {
                listing(&r.memory, all);
            }
            Ok(())
        }
        Commands::Get { id, key, resolve } => {
            let db = open_db(c)?;
            if resolve && let Some(k) = &key {
                return print_key_layers(c, &db, k);
            }
            let m = match (id, key) {
                (Some(id), _) => memoria::get(&db, &id)?,
                (None, Some(k)) => memoria::by_key(&db, &Scope::from_project(&db, &project_of(c)?)?, &k)?
                    .with_context(|| tr!("no memory with key «{}» here", "aquí no hay ninguna memoria con la clave «{}»", k.trim()))?,
                (None, None) => unreachable!("clap pide id o --key"),
            };
            if c.json {
                return json(&memoria::with_data(&db, vec![m])?.remove(0));
            }
            let data = memoria::data_of(&db, &[m.id.as_str()])?;
            print_full(&m, omoikane::diario::aplicada(&db, &m.id).ok());
            if let Some(d) = data.get(&m.id) {
                println!("\ndata: {}", serde_json::to_string_pretty(d)?);
            }
            Ok(())
        }
        Commands::Update(UpdateArgs { id, title, content, kind, paths, no_paths, tags, no_tags, pin, unpin, project, global, space, trades, no_trade, para: audience, allow_leaks, reason, replace, with, append, datos: UpdateData { data, no_data, evidence, review_reason } }) => {
            let evidence = omoikane::evidencia::clean(evidence.iter().map(|e| omoikane::evidencia::from_cli(e)).collect::<Result<Vec<_>>>()?)?;
            let data = if no_data { Some(None) } else { data.as_deref().map(memoria::data_from_text).transpose()?.map(Some) };
            anyhow::ensure!(
                replace.len() == with.len(),
                tr!("each --replace needs its --with ({} and {})", "cada --replace necesita su --with ({} y {})", replace.len(), with.len())
            );
            let changes = Changes {
                title,
                content: content.map(|t| text_or_stdin(Some(t))).transpose()?,
                kind,
                paths: if no_paths { Some(Vec::new()) } else { (!paths.is_empty()).then_some(paths) },
                tags: if no_tags { Some(Vec::new()) } else { (!tags.is_empty()).then_some(tags) },
                pinned: if pin { Some(true) } else if unpin { Some(false) } else { None },
                project: if global { Some(String::new()) } else { project.as_deref().map(proyecto::de_carpeta).transpose()? },
                reason,
                replace: replace.into_iter().zip(with).collect(),
                append,
                ..Default::default()
            };
            let (mut db, model) = open_with_model(c)?;
            let changes = Changes { space_id: space.as_deref().map(|s| proyecto::buscar_espacio(&db, s)).transpose()?, ..changes };
            let (changes, from_client) = trades_for_update(c, &db, &id, changes, &trades, no_trade, audience.as_deref(), data.as_ref(), allow_leaks)?;
            let before = omoikane::revisar::before(&db)?;
            // From a client to a trade: a new memory, and the client's one stays as it was.
            if let Some(current) = from_client {
                let tags = changes.tags.clone().unwrap_or_else(|| current.tags.clone());
                let m = omoikane::oficios::crear_de_oficio(&mut db, &current, &changes, tags, "cli", model.as_ref())?;
                if let Some(d) = data {
                    warn_masked_data(&memoria::set_data(&mut db, &m.id, d)?);
                }
                let _ = omoikane::revisar::refresh_for(&db, &before, std::slice::from_ref(&m.id));
                warn_redacted(&m);
                // The same thing promoted from another client: not prevented (it's done already), just said.
                let duplicate = match &model {
                    Some(mo) => vectores::duplicada_de_oficio(&db, mo, &omoikane::oficios::de_etiquetas(&m.tags), &m.title, &m.content, Some(&m.id))?,
                    None => None,
                };
                if c.json {
                    let mut v = memoria::with_data(&db, vec![memoria::get(&db, &m.id)?])?.remove(0);
                    v["promoted_from"] = serde_json::json!(current.id);
                    if let Some((another, sim)) = &duplicate {
                        v["duplicate_of"] = serde_json::to_value(another)?;
                        v["similarity"] = serde_json::json!((sim * 1000.0).round() / 1000.0);
                    }
                    return json(&v);
                }
                if let Some((another, sim)) = &duplicate {
                    println!(
                        "{}",
                        tr!(
                            "it looks like trade memory {} «{}» (similarity {sim:.2}): merge them (Merge in the Review tab of `omoi serve`, or `absorb` in the MCP `update`), or forget one",
                            "se parece a la memoria de oficio {} «{}» (similitud {sim:.2}): júntalas (Juntar en la pestaña Revisar de `omoi serve`, o `absorb` en el `update` del MCP), u olvida una",
                            short_id(&another.id),
                            another.title
                        )
                    );
                }
                println!(
                    "{}",
                    tr!(
                        "created trade memory {} from {}: it came from a client, so the original stays in its project (with its history); forget it if it now only repeats this one",
                        "creada la memoria de oficio {} a partir de {}: venía de un cliente, así que la original se queda en su proyecto (con su historial); olvídala si ya solo repite esta",
                        short_id(&m.id),
                        short_id(&current.id)
                    )
                );
                return Ok(());
            }
            // Only the data: no new version (like pinning it), and without `update`'s «nothing to change».
            let data_only = (data.is_some() || !evidence.is_empty() || review_reason.is_some()) && no_changes(&changes);
            let (mut m, changed) = if data_only {
                (memoria::get(&db, &id)?, Vec::new())
            } else {
                memoria::update_with_changes(&mut db, &id, changes, model.as_ref())?
            };
            if let Some(d) = data {
                warn_masked_data(&memoria::set_data(&mut db, &m.id, d)?);
            }
            if !evidence.is_empty() && omoikane::oficios::de_etiquetas(&m.tags).is_empty() && memoria::add_evidence(&mut db, &m.id, &evidence)? {
                m = memoria::get(&db, &m.id)?;
            }
            if let Some(r) = &review_reason {
                anyhow::ensure!(
                    omoikane::revisar::review_date_of(&m.tags).is_some(),
                    tr!("--review-reason goes with a review date: add --tag review:YYYY-MM-DD too", "--review-reason va con una fecha de revisión: añade también --tag review:AAAA-MM-DD")
                );
                if memoria::set_review_reason(&mut db, &m.id, r)? {
                    m = memoria::get(&db, &m.id)?;
                }
            }
            let _ = omoikane::revisar::refresh_for(&db, &before, std::slice::from_ref(&m.id));
            warn_redacted(&m);
            warn_unverified(&m);
            if c.json {
                let mut v = memoria::with_data(&db, vec![memoria::get(&db, &m.id)?])?.remove(0);
                if !changed.is_empty() {
                    v["changed"] = serde_json::json!(changed);
                }
                if let Some(u) = &m.unverified {
                    v["unverified"] = serde_json::json!(u);
                }
                return json(&v);
            }
            println!("{}", tr!("updated {}", "actualizada {}", short_id(&m.id)));
            for change in &changed {
                println!("  {}", tr!("changed:", "cambiado:"));
                for line in change.context.lines() {
                    println!("    {line}");
                }
            }
            Ok(())
        }
        Commands::Forget { id, yes, purge } => {
            let mut db = open_db(c)?;
            let m = memoria::get(&db, &id)?;
            if !yes && !confirm_forget(&m, purge)? {
                println!("{}", tr!("nothing deleted", "no se ha borrado nada"));
                return Ok(());
            }
            let before = omoikane::revisar::before(&db)?;
            let m = if purge { memoria::forget_for_good(&mut db, &m.id)? } else { memoria::forget(&mut db, &m.id)? };
            let _ = omoikane::revisar::refresh_for(&db, &before, std::slice::from_ref(&m.id));
            if c.json {
                return json(&m);
            }
            if purge {
                println!("{}", tr!("deleted for good {}: {}", "borrada del todo {}: {}", short_id(&m.id), m.title));
            } else {
                println!("{}", tr!("forgotten {}: {} (in the trash; `omoi restore {}` brings it back)", "olvidada {}: {} (en la papelera; `omoi restore {}` la devuelve)", short_id(&m.id), m.title, short_id(&m.id)));
            }
            Ok(())
        }
        Commands::Restore { id } => restore_cli(c, id),
        Commands::Verify(v) => verify_cli(c, v),
        Commands::Trash { empty, yes } => {
            let mut db = open_db(c)?;
            if !empty {
                return list_trash(c, &db);
            }
            // From a client, only what is seen from there (its space and the global ones), like the web and the TUI.
            let (visible, everything) = omoikane::http::papelera_desde(&db, &project_of(c)?)?;
            let n = visible.len();
            if !yes {
                anyhow::ensure!(omoikane::entorno::puede_preguntar(), tr!("add --yes to empty the trash without asking", "añade --yes para vaciar la papelera sin preguntar"));
                if !ask_yes_no(&tr!("Empty the trash ({n} memories)? They can't be brought back afterwards.", "¿Vaciar la papelera ({n} memorias)? Después no se pueden recuperar."), false)? {
                    return Ok(());
                }
            }
            let n = if everything {
                memoria::empty_trash(&db)?
            } else {
                let ids: Vec<String> = visible.into_iter().map(|o| o.id).collect();
                memoria::empty_trash_of(&mut db, &ids)?
            };
            if c.json {
                return json(&serde_json::json!({ "emptied": n }));
            }
            println!("{}", tr!("trash emptied: {n} memories deleted for good", "papelera vaciada: {n} memorias borradas del todo"));
            Ok(())
        }
        Commands::History { id } => {
            let db = open_db(c)?;
            let m = memoria::get(&db, &id)?;
            // From a project, only a memory seen from there (see `memoria::history_in`), with
            // its trades looked at (without them it wouldn't see the memories of its trade).
            let here = project_of(c)?;
            if let Err(e) = omoikane::oficios::preparar(&db, &here) {
                eprintln!("omoikane: trades: {e:#}");
            }
            let v = memoria::history_in(&db, &Scope::from_project(&db, &here)?, &m.id)?;
            if c.json {
                return json(&v);
            }
            print_history(&m, &v);
            Ok(())
        }
        Commands::Recent { limit, kind, all, project, space } => {
            let db = open_db(c)?;
            let scope = requested_scope(c, &db, all, project.as_deref(), space.as_deref())?;
            let all = !matches!(scope, Scope::Project { .. });
            let v = memoria::recent(&db, &scope, kind, limit)?;
            if c.json {
                return json(&v);
            }
            if v.is_empty() {
                println!("{}", tr!("nothing yet", "nada todavía"));
            }
            for m in &v {
                listing(m, all);
            }
            Ok(())
        }
        Commands::Diary(d) => diary_cli(c, d),
        Commands::Skill { id, suggest, refresh, to, force, dry_run } => skill_cli(c, id, suggest, refresh, to, force, dry_run),
        Commands::Tags { merge, all, project, space } => tags_cli(c, merge, all, project, space),
        Commands::Decisions(d) => decisions_cli(c, d),
        Commands::List(l) => {
            let db = open_db(c)?;
            let scope = requested_scope(c, &db, l.all, l.project.as_deref(), l.space.as_deref())?;
            let all = !matches!(scope, Scope::Project { .. });
            let v = memoria::with_tags(&db, &scope, &l.tags, l.kind, l.limit.unwrap_or(usize::MAX))?;
            if c.json {
                return json(&memoria::with_data(&db, v)?);
            }
            if v.is_empty() {
                println!("{}", tr!("nothing with those tags", "nada con esas etiquetas"));
            }
            for m in &v {
                listing(m, all);
            }
            Ok(())
        }
        Commands::FileHistory(h) => file_history_cli(c, h),
        Commands::Docs { accion: action } => docs_cli(c, action.unwrap_or(DocsAction::List)),
        Commands::Review { all, dismiss, kind, other, refresh_drift, refresh_trade_duplicates } => {
            let db = open_db(c)?;
            if refresh_drift {
                // The daily background process: drift and trade duplicates.
                omoikane::deriva::refresh(&db)?;
                omoikane::revisar::refresh_trade_duplicates(&db, true)?;
                return Ok(());
            }
            if refresh_trade_duplicates {
                omoikane::revisar::refresh_trade_duplicates(&db, false)?;
                return Ok(());
            }
            if let Some(id) = dismiss {
                let another = match other {
                    Some(o) => memoria::resolve_id(&db, &o)?,
                    None => String::new(),
                };
                let kind = kind.unwrap_or_else(|| if another.is_empty() { omoikane::revisar::TOO_LONG.into() } else { omoikane::revisar::OUTDATED.into() });
                omoikane::revisar::dismiss(&db, &id, &kind, &another)?;
                if kind == omoikane::revisar::CODE_DRIFT {
                    println!("{}", tr!("ok: it won't warn about it again until its files change a lot again", "hecho: no volverá a avisar hasta que sus ficheros vuelvan a cambiar mucho"));
                    return Ok(());
                }
                println!("{}", tr!("ok: it won't warn about it again until the memory changes", "hecho: no volverá a avisar hasta que la memoria cambie"));
                return Ok(());
            }
            let mut db = db;
            if omoikane::revisar::cleanup_quietly(&mut db) {
                omoikane::sync::programar(&db_path(c));
            }
            let scope = requested_scope(c, &db, all, None, None)?;
            if c.json {
                return json(&omoikane::revisar::notices(&db, &scope)?);
            }
            // Paused projects, those no longer in their folder or whose `origin` changed, and the
            // repeated ones (of those seen from here) don't depend on the warnings: they are checked at the same time, with
            // another read-only connection (each project list walks the memory table). If
            // that one can't be opened or fails, afterwards, with the usual one.
            let filter = omoikane::nombres::space_filter(&scope);
            let load_projects = |db: &Connection| -> Result<_> {
                let paused_list = proyecto::en_pausa(db)?;
                let projects = proyecto::lista(db, None, filter, usize::MAX)?;
                let proposals = omoikane::nombres::proposals_for(db, &projects)?;
                Ok((paused_list, projects, proposals))
            };
            let path = db.path().filter(|r| !r.is_empty()).map(PathBuf::from);
            let (v, side) = std::thread::scope(|s| {
                let side = s.spawn(|| {
                    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
                    path.as_ref().and_then(|r| Connection::open_with_flags(r, flags).ok()).map(|another| load_projects(&another))
                });
                (omoikane::revisar::notices(&db, &scope), side.join())
            });
            let v = v?;
            let (paused_list, projects, proposals) = match side {
                Ok(Some(Ok(x))) => x,
                _ => load_projects(&db)?,
            };
            // With thousands of warnings, `println!` wrote line by line (one system call for each).
            let mut out = std::io::BufWriter::new(std::io::stdout().lock());
            // And those in another space on another computer of the sync, with the command, and those
            // that are paused, with both.
            let clashes = omoikane::sync::choques(&db_path(c));
            for p in &projects {
                if let Some(x) = paused_list.iter().find(|x| x.key == p.key) {
                    writeln!(out, "{:<12}  {}  {}", "project_paused", p.name, p.path)?;
                    writeln!(out, "{:<12}  {}", "", omoikane::sync::texto_pausa(&db_path(c), x))?;
                }
                if p.gone {
                    writeln!(out, "{:<12}  {}  {}", "project_gone", p.name, p.path)?;
                    writeln!(out, "{:<12}  {}", "", tr!("the folder no longer exists: `omoi projects move {} <new folder>`", "la carpeta ya no existe: `omoi projects move {} <carpeta nueva>`", p.name))?;
                }
                if let Some(x) = clashes.iter().find(|x| x.key == p.key) {
                    writeln!(out, "{:<12}  {}  {}", "project_space", p.name, p.path)?;
                    writeln!(out, "{:<12}  {}", "", x.texto())?;
                }
                if !p.origin_changed.is_empty() {
                    writeln!(out, "{:<12}  {}  {}", "origin", p.name, p.path)?;
                    writeln!(out, "{:<12}  {}", "", tr!("its git origin is now {} (its sync key stays {})", "su origin de git ahora es {} (su clave de sincronización sigue siendo {})", p.origin_changed, p.key))?;
                }
            }
            // Duplicate projects or projects in temporary folders: proposed, never merged on their own.
            for x in &proposals {
                let the_other = if x.other.is_empty() { String::new() } else { format!("  →  {}  {}", x.other, x.other_path) };
                writeln!(out, "{:<12}  {}  {}{the_other}", format!("project_{}", x.kind), x.project, x.path)?;
                writeln!(out, "{:<12}  {}", "", x.detail)?;
                let no = if x.other.is_empty() { tr!("a real project:", "es un proyecto de verdad:") } else { tr!("not the same:", "no son el mismo:") };
                writeln!(out, "{:<12}  {}  ·  {no} {}", "", x.command, x.dismiss)?;
            }
            let cs = omoikane::candidatas::todas(&db)?;
            if v.is_empty() && cs.is_empty() && paused_list.is_empty() && proposals.is_empty() {
                writeln!(out, "{}", tr!("nothing to review", "nada que revisar"))?;
            }
            if !cs.is_empty() {
                writeln!(out, "{}", tr!("From your recent sessions, possibly worth keeping:", "De tus últimas sesiones, quizá valga la pena guardar:"))?;
                for x in &cs {
                    writeln!(out, "  [{}] {}", x.kind, x.text.chars().take(160).collect::<String>())?;
                }
                writeln!(out)?;
            }
            for a in &v {
                let id: String = a.memory_id.chars().take(12).collect();
                writeln!(out, "{:<12}  {id}  {}", a.kind, a.title.chars().take(90).collect::<String>())?;
                if let Some(t) = &a.other_title {
                    let label = match a.kind.as_str() {
                        omoikane::revisar::SUPERSEDED if a.via > 1 => tr!("by (via {}):", "por (via {}):", a.via),
                        omoikane::revisar::SUPERSEDED => tr!("by:", "por:"),
                        omoikane::revisar::CONTRADICTS => tr!("against:", "frente a:"),
                        _ => tr!("newer:", "nueva:"),
                    };
                    writeln!(out, "{:<12}  {label} {}  {}", "", a.other_id.chars().take(12).collect::<String>(), t.chars().take(80).collect::<String>())?;
                }
                writeln!(out, "{:<12}  {}", "", a.detail)?;
            }
            out.flush()?;
            Ok(())
        }
        Commands::Projects { accion: action } => projects_cli(c, action.unwrap_or(ProjectsAction::List)),
        Commands::Space { accion: action } => spaces_cli(c, action.unwrap_or(SpaceAction::List)),
        Commands::Trades { accion: action } => trades_cli(c, action.unwrap_or(TradesAction::List { all: false })),
        Commands::Tui { all } => {
            let (mut db, model) = open_with_model(c)?;
            let project = project_of(c)?;
            let all = all || proyecto::es_general(&project);
            omoikane::tui::ejecutar(&mut db, model.as_ref(), project, all)
        }
        Commands::Import { okf: Some(folder), space, global, trust_verified, yes, dry_run, .. } => import_okf(c, &folder, space, global, trust_verified, yes, dry_run),
        Commands::Import { obsidian: Some(vault), space, prefer, dry_run, .. } => import_obsidian(c, &vault, space, prefer.as_deref(), dry_run),
        Commands::Import { fichero: file, force, dry_run, trust_verified, .. } => {
            let file = file.context("missing file")?;
            // A network file is not opened (see `proyecto::ruta_local`).
            proyecto::exigir_local(&file)?;
            let current = project_of(c)?;
            if omoikane::copia::es_propio(&file)? {
                let path = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
                let (mut db, model) = open_with_model(c)?;
                // The verified seals, as in `import --okf`: the ones this person set; another person's,
                // only with --trust-verified; and from an agent's shell, none.
                let agent = omoikane::verificado::shell_de_agente();
                let seals = omoikane::copia::Sellos { agente: agent.is_some(), confiar: trust_verified };
                let (count, r) = omoikane::copia::importar(&mut db, &path, &file, &current, model.as_ref(), dry_run, seals)?;
                if agent.is_none() && count.untrusted_verified > 0 {
                    eprintln!(
                        "{}",
                        tr!(
                            "omoikane: {} memories carry a person's verification that you didn't put (someone else's file, or an export from a computer without your general sync repository): they come in unverified. If you trust whoever made it, import again with --trust-verified",
                            "omoikane: {} memorias llevan la verificación de una persona que no pusiste tú (un fichero de otra persona, o exportado en un equipo sin tu repositorio general de sincronización): entran sin verificar. Si te fías de quien lo hizo, vuelve a importar con --trust-verified",
                            count.untrusted_verified
                        )
                    );
                }
                if let Some(var) = agent.filter(|_| count.unverified > 0) {
                    eprintln!(
                        "{}",
                        tr!(
                            "omoikane: {} memories carry a person's verification, but this looks like a coding agent's shell ({var} is set): they come in unverified. Import again from your own terminal to keep it",
                            "omoikane: {} memorias llevan la verificación de una persona, pero esto parece la shell de un agente ({var} está puesta): entran sin verificar. Vuelve a importar desde tu terminal para conservarla",
                            count.unverified
                        )
                    );
                }
                if c.json {
                    return json(&serde_json::json!({ "file": count, "new": r.nuevas, "updated": r.actualizadas, "forgotten": r.olvidadas, "skipped_lines": r.ilegibles, "dry_run": dry_run }));
                }
                if dry_run {
                    println!("{}", tr!("{} memories, {} forget marks, {} verdicts and {} in the trash to import (the file checks out)", "{} memorias, {} marcas de olvido, {} veredictos y {} en la papelera para importar (el fichero está bien)", count.memories, count.tombstones, count.verdicts, count.forgotten));
                } else {
                    println!("{}", tr!("imported: {} new, {} updated, {} forgotten; {} lines skipped; the rest was already here", "importado: {} nuevas, {} actualizadas, {} olvidadas; {} líneas saltadas; lo demás ya estaba", r.nuevas, r.actualizadas, r.olvidadas, r.ilegibles));
                }
                if count.unknown > 0 {
                    println!("{}", omoikane::sync::aviso_lineas_nuevas(count.unknown));
                }
                if !dry_run && r.nuevas + r.actualizadas > 0 {
                    omoikane::sync::programar(&path);
                }
                return Ok(());
            }
            let (incoming, deleted) = omoikane::importar::leer(&file, c.dir.as_deref(), &current)?;
            if dry_run {
                let mut by_project: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
                for e in &incoming {
                    *by_project.entry(if e.nueva.project.is_empty() { "(global)".into() } else { e.nueva.project.clone() }).or_default() += 1;
                }
                println!("{}", tr!("{} memories to import ({deleted} deleted at the source, skipped):", "{} memorias para importar ({deleted} borradas en el origen, saltadas):", incoming.len()));
                for (p, n) in by_project {
                    println!("  {n:>5}  {p}");
                }
                return Ok(());
            }
            let (mut db, model) = open_with_model(c)?;
            let r = omoikane::importar::guardar(&mut db, model.as_ref(), incoming, force)?;
            println!(
                "{}",
                tr!(
                    "imported {}; {} skipped because they were already saved (--force to import them anyway); {} deleted at the source; {} failed",
                    "importadas {}; {} saltadas porque ya estaban (--force para importarlas igualmente); {} borradas en el origen; {} fallidas",
                    r.guardadas,
                    r.duplicadas,
                    deleted,
                    r.fallidas
                )
            );
            Ok(())
        }
        Commands::Export { okf: Some(folder), space, .. } => export_okf(c, &folder, space),
        Commands::Export { obsidian: Some(vault), space, watch, interval, .. } => {
            let db = open_db(c)?;
            let what = match space {
                Some(s) => omoikane::obsidian::Que::Espacio(proyecto::buscar_espacio(&db, &s)?),
                None => omoikane::obsidian::Que::Personal,
            };
            let json_output = c.json;
            let report = |n: &omoikane::obsidian::Cuenta| {
                if json_output {
                    println!("{}", serde_json::to_string(n).unwrap_or_default());
                } else {
                    eprintln!(
                        "{}",
                        tr!(
                            "{} memories in {}: {} new, {} changed, {} unchanged, {} deleted ({} group and file notes)",
                            "{} memorias en {}: {} nuevas, {} cambiadas, {} iguales, {} borradas ({} notas de grupo y de fichero)",
                            n.memories,
                            vault.join(omoikane::obsidian::CARPETA).display(),
                            n.created,
                            n.updated,
                            n.unchanged,
                            n.deleted,
                            n.hubs
                        )
                    );
                    if n.kept > 0 {
                        eprintln!(
                            "{}",
                            tr!(
                                "omoikane: {} notes edited in Obsidian and not imported yet were left as they are: `omoi import --obsidian {}` brings them in",
                                "omoikane: {} notas editadas en Obsidian y aún sin importar se han dejado como estaban: `omoi import --obsidian {}` las trae",
                                n.kept,
                                vault.display()
                            )
                        );
                    }
                }
            };
            if watch {
                return omoikane::obsidian::vigilar(&db, &vault, what, std::time::Duration::from_secs(interval), report);
            }
            report(&omoikane::obsidian::exportar(&db, &vault, what)?);
            Ok(())
        }
        Commands::Export { all, encrypt, .. } => {
            let mut output = std::io::BufWriter::new(std::io::stdout().lock());
            let path = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
            let db = open_db(c)?;
            // Without --all, what is seen from the project (also from the user's folder: not everything).
            let scope = if all {
                Scope::All
            } else {
                match proyecto::resolver(&db, &project_of(c)?)? {
                    Some((id, space)) => Scope::Project { id, space },
                    None => Scope::Project { id: -1, space: None },
                }
            };
            let n = omoikane::copia::exportar(&db, &path, &scope, encrypt, &mut output)?;
            output.flush()?;
            eprintln!("{}", tr!("{} memories exported (with {} forget marks, {} verdicts and {} in the trash)", "{} memorias exportadas (con {} marcas de olvido, {} veredictos y {} en la papelera)", n.memories, n.tombstones, n.verdicts, n.forgotten));
            Ok(())
        }
        Commands::Backup { list } => {
            let path = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
            if !list {
                let target = omoikane::copia::copiar(&open_db(c)?, &path)?;
                if !c.json {
                    println!("{}", tr!("copied to {}", "copiada en {}", target.display()));
                }
            }
            let v = omoikane::copia::copias(&path);
            if c.json {
                return json(&v.iter().map(|(p, b)| serde_json::json!({ "path": p, "bytes": b })).collect::<Vec<_>>());
            }
            if list {
                if v.is_empty() {
                    println!("{}", tr!("no copies yet", "todavía no hay copias"));
                }
                for (p, b) in v {
                    println!("{}  {} KB", p.display(), b / 1024);
                }
            }
            Ok(())
        }
    }
}

/// `omoi upgrade`.
fn upgrade(check_only: bool, on: bool, off: bool, background: bool, after: Option<String>) -> Result<()> {
    use omoikane::actualizar::{self, Resultado};
    if let Some(previous) = after {
        actualizar::despues(&previous)?;
        return Ok(());
    }
    if on || off {
        actualizar::fijar(on)?;
        if omoikane::setup::instalado() {
            let _ = omoikane::setup::auto_update(on);
        }
        println!(
            "{}",
            if on {
                tr!("Automatic update on: once a day omoi checks for a newer signed release and installs it.", "Actualización automática encendida: una vez al día omoi mira si hay una Release firmada más nueva y la instala.")
            } else {
                tr!("Automatic update off. `omoi upgrade` updates by hand.", "Actualización automática apagada. `omoi upgrade` actualiza a mano.")
            }
        );
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let r = actualizar::comprobar(&exe, !check_only);
    if background {
        return Ok(()); // nobody reads it: whatever happens shows up in the next session
    }
    match r? {
        Resultado::AlDia(v) => println!("{}", tr!("omoi {v} is the latest version.", "omoi {v} es la última versión.")),
        Resultado::NoSeInstala(v, reason) if reason.is_empty() => println!("{}", tr!("omoi {v} is available: `omoi upgrade` installs it.", "Hay omoi {v}: `omoi upgrade` la instala.")),
        Resultado::NoSeInstala(v, reason) => println!("{}", tr!("omoi {v} is available, but not installed automatically: {reason}", "Hay omoi {v}, pero no se instala sola: {reason}")),
        Resultado::Actualizado(v) => println!("{}", tr!("Updated to omoi {v}.", "Actualizado a omoi {v}.")),
    }
    Ok(())
}

/// What is asked when setting up Claude Code: whether Omoikane updates itself (the binary once a day
/// and the plugin with the marketplace's automatic update). Without a terminal or with `--yes`, yes.
fn ask_auto_update(yes: bool, no: bool, dry_run: bool) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    let on = if no {
        false
    } else if let Some(already) = omoikane::actualizar::preferencia().filter(|_| yes || !omoikane::entorno::puede_preguntar()) {
        already
    } else if yes || !omoikane::entorno::puede_preguntar() {
        // Without asking it is not turned on: it has to be a yes from the person.
        false
    } else {
        ask_yes_no(
            &tr!(
                "Keep Omoikane up to date by itself? (once a day it installs a newer release signed with Omoikane's key, and Claude Code updates the plugin)",
                "¿Quieres que Omoikane se actualice solo? (una vez al día instala una Release más nueva firmada con la clave de Omoikane, y Claude Code actualiza el plugin)"
            ),
            false,
        )?
    };
    omoikane::actualizar::fijar(on)?;
    omoikane::setup::auto_update(on)?;
    println!(
        "{}",
        if on {
            tr!("Automatic update: on (`omoi upgrade --off` turns it off).", "Actualización automática: encendida (`omoi upgrade --off` la apaga).")
        } else {
            tr!("Automatic update: off (`omoi upgrade --on` turns it on).", "Actualización automática: apagada (`omoi upgrade --on` la enciende).")
        }
    );
    Ok(())
}

fn setup(agent: &str, yes: bool, dry_run: bool, no_auto_update: bool) -> Result<()> {
    if agent != "claude" {
        return setup_other(agent, yes, dry_run);
    }
    let steps = omoikane::setup::plan()?;
    if steps.is_empty() {
        println!("{}", tr!("Claude Code already has Omoikane: nothing to do.", "Claude Code ya tiene Omoikane: nada que hacer."));
        if omoikane::actualizar::preferencia().is_none() || no_auto_update {
            ask_auto_update(yes, no_auto_update, dry_run)?;
        }
        return Ok(());
    }
    println!("{}", tr!("To set up Claude Code, omoi will:", "Para preparar Claude Code, omoi va a:"));
    for p in &steps {
        println!("  - {}   (claude {})", p.que, p.args.join(" "));
    }
    if dry_run {
        return Ok(());
    }
    if !yes && !ask()? {
        println!("{}", tr!("nothing changed", "no se ha cambiado nada"));
        return Ok(());
    }
    omoikane::setup::ejecutar(&steps)?;
    ask_auto_update(yes, no_auto_update, dry_run)?;
    println!(
        "{}",
        tr!(
            "Done. Open a new Claude Code session: the memory tools and the automatic recall are there.",
            "Hecho. Abre una sesión nueva de Claude Code: ahí están las herramientas de memoria y el recall automático."
        )
    );
    Ok(())
}

/// `omoi recall --session-end`: notes the session in the diary and sets aside candidates from the conversation.
/// A failure never gets in the way.
fn recall_on_session_end(c: &Common) -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let mut e: recall::HookInput = serde_json::from_str(&input).unwrap_or_default();
    let db_path = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
    // Cursor gives the folder in `workspace_roots`; Copilot and Kiro don't give the conversation's path.
    if e.cwd.is_empty()
        && let Some(root) = e.workspace_roots.first()
    {
        e.cwd = root.clone();
    }
    if e.transcript_path.is_empty()
        && let Some(p) = omoikane::candidatas::buscar_conversacion(&e.session_id)
    {
        e.transcript_path = p.to_string_lossy().into_owned();
    }
    let conversation = Some(std::path::Path::new(&e.transcript_path)).filter(|p| !e.transcript_path.is_empty() && proyecto::existe(p));
    if (conversation.is_none() && e.session_id.trim().is_empty()) || !db_path.exists() {
        return Ok(());
    }
    let project = match (&c.dir, e.cwd.is_empty()) {
        (Some(d), _) => proyecto::de(d)?,
        // The hook's `cwd` is the folder of the agent's own session: it may be on the network (see
        // `proyecto::de_confianza`). `--dir` comes from outside and goes through `proyecto::de`.
        (None, false) => proyecto::de_confianza(std::path::Path::new(&e.cwd))?,
        (None, true) => proyecto::actual()?,
    };
    let db = db::abrir(&db_path)?;
    // The diary first: it is the fastest, and if the hook is cut off, it stays noted. Neither an error nor a
    // panic of its own cuts the hook or leaves the candidates unset aside.
    let diary = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| omoikane::diario::apuntar(&db, &project, &e.session_id, conversation)));
    match diary {
        Ok(Err(err)) => eprintln!("omoikane: {err:#}"),
        Err(_) => eprintln!("omoikane: the session diary failed"),
        Ok(Ok(_)) => {}
    }
    // The conversation is read once for the candidates and for the recipe steps.
    let turns = conversation.and_then(|r| omoikane::candidatas::turnos(r).map_err(|err| eprintln!("omoikane: {err:#}")).ok());
    // The memories the agent said it followed, citing their id: they go to the diary line.
    if let Some(t) = &turns {
        let texts = t.iter().filter_map(|x| match x {
            omoikane::candidatas::Turno::Agente(s) => Some(s.as_str()),
            _ => None,
        });
        let applied_result = omoikane::diario::citadas(&db, texts)
            .and_then(|ids| omoikane::diario::apuntar_aplicadas(&db, &project, &e.session_id, conversation, &ids));
        if let Err(err) = applied_result {
            eprintln!("omoikane: {err:#}");
        }
    }
    let steps = turns.as_deref().map(omoikane::recetas::pasos_de_turnos).unwrap_or_default();
    if let (Some(path), Some(t)) = (conversation, turns)
        && let Err(err) = omoikane::candidatas::apartar_con(&db, &project, &e.session_id, path, t)
    {
        eprintln!("omoikane: {err:#}");
    }
    // Last: the session's steps and, if some sequence already repeats enough, a recipe
    // proposed for the next one. It never cuts the hook either.
    if conversation.is_some() {
        let recipes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            omoikane::recetas::apuntar(&db, &e.session_id, &steps)?;
            omoikane::recetas::proponer(&db, &project, &e.session_id, &steps)?;
            Ok(())
        }));
        match recipes {
            Ok(Err(err)) => eprintln!("omoikane: {err:#}"),
            Err(_) => eprintln!("omoikane: proposing recipes failed"),
            Ok(Ok(())) => {}
        }
    }
    Ok(())
}

fn recall(c: &Common, on_session_start: bool, on_file_open: bool, hook_format: recall::HookFormat) -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let mut e: recall::HookInput = serde_json::from_str(&input).unwrap_or_default();
    if e.cwd.is_empty()
        && let Some(root) = e.workspace_roots.first()
    {
        e.cwd = root.clone();
    }
    if e.prompt.is_empty()
        && let Ok(p) = std::env::var("USER_PROMPT")
    {
        e.prompt = p;
    }
    let project = match (&c.dir, e.cwd.is_empty()) {
        (Some(d), _) => proyecto::de(d)?,
        // The hook's `cwd` is the folder of the agent's own session: it may be on the network (see
        // `proyecto::de_confianza`). `--dir` comes from outside and goes through `proyecto::de`.
        (None, false) => proyecto::de_confianza(std::path::Path::new(&e.cwd))?,
        (None, true) => proyecto::actual()?,
    };
    let path = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
    if !path.exists() && !on_session_start {
        return Ok(()); // no memories yet: nothing to inject
    }
    let mut db = db::abrir(&path)?;
    let (event, text) = if on_file_open {
        ("PreToolUse", recall::on_file_open(&mut db, &project, &e)?)
    } else if on_session_start {
        // Once a day: the database backup and purging the trash.
        omoikane::copia::diaria(&db, &path);
        let general = matches!(omoikane::sync::leer_config(&path), Ok(Some(_)));
        if general {
            let _ = omoikane::sync::adoptar(&db, &project);
        }
        // With the general repository or with some space's (even if there are only spaces).
        if general || !omoikane::sync::espacios(&path).is_empty() {
            omoikane::sync::programar(&path);
            // That sync already brings what's new: the first message doesn't launch another.
            omoikane::sync::apuntar_traido(&db);
        }
        let text = recall::on_session_start(&mut db, &project, &e)?.map(|t| match omoikane::sync::aviso(&path) {
            // Before the block's closing, which is the last `</omoikane>`.
            Some(a) => match t.rfind("</omoikane>") {
                Some(i) => format!("{}{a}\n{}", &t[..i], &t[i..]),
                None => t,
            },
            None => t,
        });
        ("SessionStart", text)
    } else {
        // The model opens in milliseconds (mapped), and only if the message reaches the meaning
        // channel; without it, recall goes by words only.
        let model = std::cell::OnceCell::new();
        let mut open_model = || model.get_or_init(|| modelos::abrir_actual().ok().flatten()).as_ref();
        ("UserPromptSubmit", recall::on_prompt_opening(&mut db, &project, &e, &mut open_model)?)
    };
    if let Some(t) = text {
        println!("{}", recall::hook_output_in(hook_format, event, &t));
    }
    // What other computers pushed mid-session: every 2 minutes at most, on the side and without
    // waiting for it (without sync, nothing).
    if on_file_open {
        // Nothing else: opening files must not cost a sync.
    } else if !on_session_start {
        omoikane::sync::traer_si_toca(&db, &path);
    } else {
        // And the index of the project's documentation, also on the side.
        omoikane::documentos::refrescar_aparte(&db, &project, &path);
    }
    Ok(())
}

/// The database and the model, with the vectors up to date (if the model changed or they are missing, they are computed now).
fn open_with_model(c: &Common) -> Result<(Connection, Option<Modelo>)> {
    let mut db = open_db(c)?;
    let model = modelos::abrir_actual()?;
    if let Some(m) = &model {
        vectores::sincronizar(&mut db, m)?;
    }
    Ok((db, model))
}

/// What state the meaning model is in for the database `path` and why, as JSON and as lines for
/// `omoi doctor` and `omoi model semantic`.
fn meaning_report(path: &std::path::Path) -> Result<(serde_json::Value, Vec<String>)> {
    use omoikane::significado::{self as s, Porque};
    let db = if path.exists() { Some(db::abrir(path)?) } else { None };
    let e = match &db {
        Some(db) => s::estado(db),
        // No database yet: the environment and the choice count the same; if there is neither, 0 memories.
        None => match (std::env::var("OMOIKANE_SEMANTIC"), s::preferencia()) {
            (Ok(v), _) if !v.is_empty() => s::Estado { on: v != "0", reason: Porque::Env, memories: 0, threshold: s::umbral() },
            (_, Some(p)) => s::Estado { on: p, reason: Porque::Chosen, memories: 0, threshold: s::umbral() },
            _ => s::Estado { on: false, reason: Porque::Size, memories: 0, threshold: s::umbral() },
        },
    };
    let installed = s::modelo();
    let downloading = installed.is_none() && s::descargando();
    let running = if e.on && installed.is_some() { s::vivo(std::time::Duration::from_millis(300)) } else { None };
    let (done, total) = match (&db, e.on && installed.is_some()) {
        (Some(db), true) => s::cobertura(db)?,
        _ => (0, e.memories),
    };
    let (queries, unanswered, last_miss) = db.as_ref().map(omoikane::recall::model_queries).unwrap_or((0, 0, None));
    let (n, u) = (e.memories, e.threshold);
    let why = match e.reason {
        Porque::Env => "OMOIKANE_SEMANTIC".to_string(),
        Porque::Chosen => tr!("you turned it on", "lo encendiste tú"),
        Porque::Size => tr!("{n} memories; it turns on by itself from {u}", "{n} memorias; se enciende solo desde {u}"),
    };
    let first = match (e.on, e.reason, &installed) {
        (false, Porque::Env, _) => tr!("off: OMOIKANE_SEMANTIC=0", "apagado: OMOIKANE_SEMANTIC=0"),
        (false, Porque::Chosen, _) => tr!(
            "off: you turned it off (`omoi model semantic --auto` lets the size of the memory decide, `--on` uses it now)",
            "apagado: lo apagaste tú (`omoi model semantic --auto` deja que decida el tamaño de la memoria, `--on` lo usa ya)"
        ),
        (false, _, i) => {
            let idle = if i.is_some() { tr!(" It is installed but idle.", " Está instalado, sin usar.") } else { String::new() };
            tr!(
                "not in use yet: {n} memories, it turns on by itself from {u} (below that, words alone are as good and faster). `omoi model semantic --on` uses it now, `--off` never.{idle}",
                "aún sin usar: {n} memorias, se enciende solo desde {u} (por debajo, las palabras solas aciertan igual y van más rápido). `omoi model semantic --on` lo usa ya, `--off` nunca.{idle}"
            )
        }
        (true, _, None) if downloading => tr!(
            "on ({why}): downloading EmbeddingGemma (about 450 MB) in the background; until then, words only",
            "encendido ({why}): descargando EmbeddingGemma (unos 450 MB) en segundo plano; mientras, solo palabras"
        ),
        (true, _, None) => tr!(
            "on ({why}), but not installed: it downloads at the next session start (`omoi model install --semantic` does it now, about 450 MB)",
            "encendido ({why}), pero sin instalar: se descarga al empezar la próxima sesión (`omoi model install --semantic` lo hace ya, unos 450 MB)"
        ),
        (true, _, Some(_)) => match running.as_ref().map(|r| r.pid) {
            Some(pid) => tr!(
                "on ({why}): EmbeddingGemma, {done} of {total} memories with a vector; service running (pid {pid})",
                "encendido ({why}): EmbeddingGemma, {done} de {total} memorias con vector; servicio en marcha (pid {pid})"
            ),
            None => tr!(
                "on ({why}): EmbeddingGemma, {done} of {total} memories with a vector; the service starts with the next search",
                "encendido ({why}): EmbeddingGemma, {done} de {total} memorias con vector; el servicio arranca con la próxima búsqueda"
            ),
        },
    };
    let mut lines = vec![first];
    if queries > 0 {
        let last_miss = last_miss.clone().unwrap_or_default();
        lines.push(if unanswered > 0 {
            tr!(
                "recall asked it {queries} times; {unanswered} got no answer in time and went by words only, as in 1.1 (last: {last_miss})",
                "el recall le preguntó {queries} veces; {unanswered} sin respuesta a tiempo, que fueron solo por palabras, como en la 1.1 (la última: {last_miss})"
            )
        } else {
            tr!("recall asked it {queries} times, always answered in time", "el recall le preguntó {queries} veces, siempre respondió a tiempo")
        });
    }
    let (ram_mb, ram_source) = s::tope_ram();
    lines.push(ram_in_use(ram_mb, ram_source));
    let j = serde_json::json!({
        "on": e.on,
        "reason": e.reason,
        "memories": e.memories,
        "threshold": e.threshold,
        "choice": s::preferencia(),
        "installed": installed.as_ref().map(|d| d.display().to_string()),
        "downloading": downloading,
        "with_vector": done,
        "service_pid": running.as_ref().map(|r| r.pid),
        "recall_asked": queries,
        "recall_no_answer": unanswered,
        "recall_last_no_answer": last_miss,
        "indexing_ram_mb": ram_mb,
        "indexing_ram_from": ram_source,
    });
    Ok((j, lines))
}

fn model(c: &Common, action: ModelAction) -> Result<()> {
    match action {
        ModelAction::Status => {
            let v = modelos::instalados();
            if c.json {
                return json(&v);
            }
            if v.is_empty() {
                println!(
                    "{}",
                    tr!(
                        "no model installed: search works by words only. Install it with `omoi model install`",
                        "no hay modelo instalado: la búsqueda va solo por palabras. Instálalo con `omoi model install`"
                    )
                );
            }
            for (name, current) in v {
                println!("{} {name}", if current { "*" } else { " " });
            }
            println!("{}", modelos::carpeta().display());
            Ok(())
        }
        ModelAction::Install { from, semantic: true } => {
            use omoikane::significado as s;
            let dir = s::instalar(from.as_deref(), &mut |line| eprintln!("{line}"))?;
            s::descarga_hecha();
            // If this database uses it, let the service start computing its vectors right away.
            let path = db_path(c);
            if s::en_uso_en(&path) {
                s::avisar(&path);
            }
            if c.json {
                return json(&serde_json::json!({ "semantic_model": dir.display().to_string() }));
            }
            println!("{}", tr!("meaning model ready: {}", "modelo de significado listo: {}", dir.display()));
            Ok(())
        }
        ModelAction::Install { from, semantic: false } => {
            let id = modelos::instalar(from.as_deref(), &mut |line| eprintln!("{line}"))?;
            // The database's vectors are computed now, so the first search doesn't wait for them.
            let mut db = open_db(c)?;
            let n = match modelos::abrir_actual()? {
                Some(m) => vectores::sincronizar(&mut db, &m)?,
                None => 0,
            };
            if c.json {
                return json(&serde_json::json!({ "model": id, "vectors": n }));
            }
            println!("{}", tr!("model ready: {id} ({n} vectors computed)", "modelo listo: {id} ({n} vectores calculados)"));
            Ok(())
        }
        ModelAction::ServeSemantic => omoikane::significado::servir(c.db.as_deref()),
        ModelAction::IndexSemantic => {
            use omoikane::significado as s;
            let dir = s::modelo().context(tr!("the meaning model is not installed (`omoi model install --semantic`)", "el modelo de significado no está instalado (`omoi model install --semantic`)"))?;
            let gemma = omoikane::gemma::Gemma::cargar(&dir)?;
            let db = open_db(c)?;
            let (_, total) = s::cobertura(&db)?;
            let start = std::time::Instant::now();
            let n = s::indexar(&db, &gemma, 64, &mut || true, &mut |n| {
                if n % 100 == 0 {
                    eprintln!("{n}/{total} ({:.0} s)", start.elapsed().as_secs_f64());
                }
            })?;
            let (done, total) = s::cobertura(&db)?;
            if c.json {
                return json(&serde_json::json!({ "computed": n, "with_vector": done, "memories": total, "seconds": start.elapsed().as_secs_f64() }));
            }
            println!("{}", tr!("{n} vectors computed; {done} of {total} memories have one", "{n} vectores calculados; {done} de {total} memorias tienen el suyo"));
            Ok(())
        }
        ModelAction::Semantic { on, off, auto, ram } => {
            use omoikane::significado as s;
            if on || off || auto {
                s::fijar(if auto { None } else { Some(on) })?;
            }
            if let Some(mb) = ram {
                s::fijar_ram((mb > 0).then_some(mb))?;
            }
            let path = db_path(c);
            let (j, lines) = meaning_report(&path)?;
            // On (by hand, or by size when going back to `--auto`) and not installed: download it now.
            if (on || auto) && j["on"].as_bool() == Some(true) && s::modelo().is_none() {
                s::descargar_aparte(&path);
            }
            if c.json {
                return json(&j);
            }
            for l in lines {
                println!("{l}");
            }
            Ok(())
        }
        ModelAction::Clean => {
            let removed = modelos::limpiar();
            if c.json {
                return json(&removed);
            }
            println!("{}", tr!("{} models deleted", "{} modelos borrados", removed.len()));
            Ok(())
        }
    }
}

fn open_db(c: &Common) -> Result<Connection> {
    db::abrir(&c.db.clone().unwrap_or_else(db::ruta_por_defecto))
}

fn tags_cli(c: &Common, merge: Option<Vec<String>>, all: bool, project: Option<String>, space: Option<String>) -> Result<()> {
    let mut db = open_db(c)?;
    let scope = requested_scope(c, &db, all, project.as_deref(), space.as_deref())?;
    if let Some([from, a]) = merge.as_deref() {
        let n = omoikane::etiquetas::merge(&mut db, &scope, from, a)?;
        if c.json {
            return json(&serde_json::json!({ "from": from, "to": a, "changed": n }));
        }
        println!("{}", tr!("«{}» → «{}» in {} memories", "«{}» → «{}» en {} memorias", from, a, n));
        return Ok(());
    }
    let pairs = omoikane::etiquetas::variant_pairs(&db, &scope)?;
    if c.json {
        let v: Vec<_> = pairs.iter().map(|(a, na, b, nb)| serde_json::json!({ "tag": a, "count": na, "variant": b, "variant_count": nb })).collect();
        return json(&v);
    }
    if pairs.is_empty() {
        println!("{}", tr!("no tags here are variants of each other", "aquí no hay etiquetas que sean variantes unas de otras"));
        return Ok(());
    }
    for (a, na, b, nb) in &pairs {
        println!("{a} ({na})  ~  {b} ({nb})");
        println!("    omoi tags --merge {b} {a}");
    }
    Ok(())
}

fn skill_cli(c: &Common, id: Option<String>, suggest: bool, refresh: bool, to: Option<String>, force: bool, dry_run: bool) -> Result<()> {
    let mut db = open_db(c)?;
    let project = project_of(c)?;
    let written = match (&id, refresh) {
        (Some(asked), _) => {
            let id = omoikane::skill::resolver(&mut db, &project, asked)?;
            // If it was asked for with words, show which one was chosen.
            if !c.json && !id.starts_with(&asked.trim().to_uppercase()) {
                let m = memoria::get(&db, &id)?;
                println!("{}", tr!("memory {}  {}", "memoria {}  {}", short_id(&id), m.title));
            }
            omoikane::skill::escribir(&db, &project, &id, omoikane::skill::Destino::de(to.as_deref())?, force, dry_run)?
        }
        (None, true) => omoikane::skill::refrescar(&db, &project, dry_run)?,
        (None, false) => {
            let skills = if suggest { Vec::new() } else { omoikane::skill::listar(&db, &project)? };
            let ideas = omoikane::skill::sugerencias(&db, &project)?;
            if c.json {
                return if suggest { json(&ideas) } else { json(&skills) };
            }
            if skills.is_empty() && !suggest {
                println!("{}", tr!("no skills from Omoikane in this repository yet", "aún no hay skills de Omoikane en este repositorio"));
            }
            for s in &skills {
                let state = match s.state {
                    omoikane::skill::Estado::UpToDate => tr!("up to date", "al día"),
                    omoikane::skill::Estado::Outdated => tr!("outdated: the memory changed (omoi skill --refresh)", "desfasada: la memoria cambió (omoi skill --refresh)"),
                    omoikane::skill::Estado::Edited => tr!("edited by hand", "editada a mano"),
                    omoikane::skill::Estado::Orphan => tr!("its memory no longer exists", "su memoria ya no existe"),
                };
                let short_id: String = s.memory_id.chars().take(12).collect();
                println!("{}  {short_id}  {state}", s.path);
            }
            if ideas.is_empty() {
                if suggest || skills.is_empty() {
                    println!("{}", tr!(
                        "No memory of this project stands out as a skill yet (procedures, or memories applied in 2 sessions or more). Any memory works: `omoi skill \"a few words\"` or `omoi skill <id>`.",
                        "Aún no destaca ninguna memoria del proyecto como skill (recetas, o memorias aplicadas en 2 sesiones o más). Vale cualquiera: `omoi skill \"unas palabras\"` u `omoi skill <id>`."
                    ));
                }
                return Ok(());
            }
            if !suggest {
                println!();
            }
            println!("{}", tr!("Could be skills:", "Podrían ser skills:"));
            for i in &ideas {
                let why = match (i.kind == omoikane::memoria::Kind::Procedure, i.applied_sessions) {
                    (_, 1) => tr!("applied in 1 session", "aplicada en 1 sesión"),
                    (_, n) if n > 1 => tr!("applied in {n} sessions", "aplicada en {n} sesiones"),
                    _ => tr!("procedure", "receta"),
                };
                println!("  omoi skill {}   {}  ({why})", short_id(&i.id), i.title);
            }
            return Ok(());
        }
    };
    if c.json {
        return json(&written);
    }
    if written.is_empty() {
        println!("{}", tr!("nothing to bring up to date", "nada que poner al día"));
    }
    for e in &written {
        let what = match (e.action, dry_run) {
            ("created", false) => tr!("created", "creada"),
            ("created", true) => tr!("would create", "se crearía"),
            ("updated", false) => tr!("updated", "puesta al día"),
            ("updated", true) => tr!("would update", "se pondría al día"),
            ("unchanged", _) => tr!("already up to date", "ya estaba al día"),
            _ => tr!("skipped", "no se toca"),
        };
        match &e.reason {
            Some(r) => println!("{}  {what}: {r}", e.path),
            None => println!("{}  {what}", e.path),
        }
    }
    if !dry_run && written.iter().any(|e| matches!(e.action, "created" | "updated")) {
        println!("{}", tr!("Commit it so the team and every agent get it.", "Haz commit para que lo tengan el equipo y todos los agentes."));
    }
    Ok(())
}

fn project_of(c: &Common) -> Result<String> {
    match &c.dir {
        Some(d) => proyecto::de(d),
        None => proyecto::actual(),
    }
}

/// What is seen from the folder (`--dir` or the current one), or what is asked for (`--all`, `--project`,
/// `--space`), without leaving its space.
/// `omoi decisions`: the open decisions, blocking ones first.
fn decisions_cli(c: &Common, d: DecisionsArgs) -> Result<()> {
    let mut db = open_db(c)?;
    if let (Some(id), Some(answer)) = (&d.resolve, &d.answer) {
        // Only one seen from here: another client's is not resolved.
        let id = memoria::resolve_id(&db, id)?;
        anyhow::ensure!(
            Scope::from_project(&db, &project_of(c)?)?.sees(&db, &id)?,
            tr!("{id} is in another space: resolve it from that project", "{id} es de otro espacio: resuélvela desde ese proyecto")
        );
        let before = omoikane::revisar::before(&db)?;
        let m = match memoria::resolve_decision(&mut db, &id, answer, d.reason, d.allow_leaks)? {
            Ok(m) => m,
            Err(leaks) => {
                warn_leaks(c, &leaks)?;
                unreachable!()
            }
        };
        let _ = omoikane::revisar::refresh_for(&db, &before, std::slice::from_ref(&m.id));
        omoikane::sync::programar(&db_path(c));
        if c.json {
            return json(&memoria::with_data(&db, vec![m])?.remove(0));
        }
        println!("{}", tr!("resolved {}: {}", "resuelta {}: {}", short_id(&m.id), m.title));
        return Ok(());
    }
    if omoikane::revisar::cleanup_quietly(&mut db) {
        omoikane::sync::programar(&db_path(c));
    }
    let scope = requested_scope(c, &db, d.all, d.project.as_deref(), d.space.as_deref())?;
    let v = memoria::decisions(&db, &scope, d.blocking, &d.tags, d.limit)?;
    if c.json {
        return json(&v);
    }
    if v.is_empty() {
        println!("{}", tr!("no open decisions", "no hay decisiones abiertas"));
    }
    let with_project = !matches!(scope, Scope::Project { .. });
    for x in &v {
        let mark = if x.priority == "blocking" { tr!("BLOCKING", "BLOQUEANTE") } else { tr!("later", "después") };
        println!("{}  {:<10} {}", short_id(&x.id), mark, x.title);
        let location = if x.global {
            "global".to_string()
        } else if x.project.is_empty() {
            format!("◇ {}", x.space)
        } else {
            x.project.clone()
        };
        if with_project || x.project.is_empty() {
            println!("              {location}");
        }
        println!("              {}", one_line(&x.content, 160));
    }
    Ok(())
}

/// `omoi file-history`: the history of some files (those of a diff, through the input) for the risk
/// gate.
fn file_history_cli(c: &Common, h: FileHistoryArgs) -> Result<()> {
    use std::io::IsTerminal;
    let mut files: Vec<String> = h.files.into_iter().filter(|f| f != "-").collect();
    if files.is_empty() {
        let input = std::io::stdin();
        anyhow::ensure!(
            !input.is_terminal(),
            tr!(
                "which files? Pass them as arguments, or through the standard input (git diff --name-only | omoi file-history)",
                "¿qué ficheros? Pásalos como argumentos o por la entrada estándar (git diff --name-only | omoi file-history)"
            )
        );
        let mut t = String::new();
        input.lock().read_to_string(&mut t)?;
        files = t.lines().map(str::to_string).collect();
    }
    let project = project_of(c)?;
    let db = open_db(c)?;
    let r = omoikane::riesgo::de_ficheros(&db, &project, &files)?;
    if c.json {
        return json(&r);
    }
    for f in &r.files {
        match &f.last {
            None => print!("{}: {}", f.file, tr!("not in the session diary", "no sale en el diario de sesiones")),
            Some(u) => {
                let n = f.sessions;
                print!("{}: {}", f.file, tr!("changed in {n} of {} sessions (last {u})", "cambió en {n} de {} sesiones (la última, {u})", r.sessions));
            }
        }
        if !f.changes_with.is_empty() {
            let v: Vec<String> = f.changes_with.iter().map(|j| format!("{} ({})", j.file, j.sessions)).collect();
            print!("; {} {}", tr!("with", "con"), v.join(", "));
        }
        if f.gotchas > 0 {
            let ids: Vec<&str> = f.gotcha_ids.iter().map(|i| short_id(i)).collect();
            print!("; {} gotcha(s): {}", f.gotchas, ids.join(", "));
        }
        println!();
    }
    Ok(())
}

/// `omoi diary`.
fn diary_cli(c: &Common, d: DiaryArgs) -> Result<()> {
    use omoikane::diario;
    let DiaryArgs { limit, since, until, file, all, project, space } = d;
    let path = db_path(c);
    if !path.exists() {
        if c.json {
            return json(&Vec::<diario::Linea>::new());
        }
        println!("{}", tr!("nothing yet", "nada todavía"));
        return Ok(());
    }
    let db = db::abrir(&path)?;
    let scope = requested_scope(c, &db, all, project.as_deref(), space.as_deref())?;
    let filter = diario::Filtro {
        desde: since.as_deref().map(|d| diario::fecha(&db, d)).transpose()?,
        hasta: until.as_deref().map(|d| diario::fecha(&db, d)).transpose()?,
        fichero: file,
        // Without `--project`, from the folder: its lines from before it was a project too.
        carpeta: if project.is_none() { Some(project_of(c)?) } else { None },
        limite: limit,
    };
    let v = diario::lineas(&db, &scope, &filter)?;
    if c.json {
        return json(&v);
    }
    if v.is_empty() {
        println!("{}", tr!("nothing yet", "nada todavía"));
    }
    let several = !matches!(scope, Scope::Project { .. });
    for l in &v {
        let time_of = |t: &str| t.get(11..16).unwrap_or("").to_string();
        let day = l.first_at.get(..10).unwrap_or("");
        let end = if l.last_at.get(..10) == Some(day) { time_of(&l.last_at) } else { format!("{} {}", l.last_at.get(..10).unwrap_or(""), time_of(&l.last_at)) };
        let location = if several { format!("  {}", proyecto::nombre_de(&l.project, "")) } else { String::new() };
        println!("{day} {}–{end} UTC{location}  [{}]", time_of(&l.first_at), l.session.chars().take(8).collect::<String>());
        if !l.files.is_empty() {
            println!("  {} {}", tr!("files:", "ficheros:"), l.files.join(", "));
        }
        for (what, ms) in [(tr!("recalled:", "recibidas:"), &l.recalled), (tr!("applied:", "aplicadas:"), &l.applied), (tr!("saved:", "guardadas:"), &l.saved)] {
            for m in ms.iter() {
                let short_id: String = m.id.chars().take(12).collect();
                match &m.title {
                    Some(t) => println!("  {what} {short_id} {t}"),
                    None => println!("  {what} {short_id} {}", tr!("(forgotten)", "(olvidada)")),
                }
            }
        }
    }
    Ok(())
}

/// `omoi docs`.
fn docs_cli(c: &Common, action: DocsAction) -> Result<()> {
    use omoikane::documentos as d;
    let project = project_of(c)?;
    let path = db_path(c);
    // A relative path is relative to `--dir`, if given (like the project).
    let next_to_dir = |p: &std::path::Path| match &c.dir {
        Some(d) if p.is_relative() => d.join(p),
        _ => p.to_path_buf(),
    };
    // Looking doesn't create the database.
    if !path.exists() && matches!(action, DocsAction::List | DocsAction::Remove { .. } | DocsAction::Refresh | DocsAction::Test { .. }) {
        if c.json {
            return match action {
                DocsAction::Remove { .. } => json(&serde_json::json!({ "removed": false })),
                DocsAction::Test { .. } => json(&None::<d::Encontrado>),
                _ => json(&Vec::<d::Carpeta>::new()),
            };
        }
        println!("{}", tr!("no documentation folders yet (`omoi docs add <folder>`)", "aún no hay carpetas de documentación (`omoi docs add <carpeta>`)"));
        return Ok(());
    }
    let db = db::abrir(&path)?;
    match action {
        DocsAction::List => {
            let v = d::listar(&db, &project)?;
            if c.json {
                return json(&v);
            }
            if v.is_empty() {
                println!("{}", tr!("no documentation folders yet (`omoi docs add <folder>`)", "aún no hay carpetas de documentación (`omoi docs add <carpeta>`)"));
            }
            for x in &v {
                println!("{}  {}", x.folder, tr!("({} files, {} sections)", "({} ficheros, {} apartados)", x.files, x.chunks));
            }
            Ok(())
        }
        DocsAction::Add { carpeta: folder } => {
            let (folder, r) = d::anadir(&db, &project, &next_to_dir(&folder))?;
            if c.json {
                return json(&serde_json::json!({ "folder": folder, "refresh": r }));
            }
            println!("{}", tr!("added {folder}: {} files indexed", "añadida {folder}: {} ficheros indexados", r.indexed + r.unchanged));
            if r.skipped > 0 {
                println!("{}", tr!("  {} skipped (over 2 MB, unreadable, or past {} files)", "  {} sin indexar (más de 2 MB, ilegibles o pasados {} ficheros)", r.skipped, d::MAX_FICHEROS));
            }
            Ok(())
        }
        DocsAction::Remove { carpeta: folder } => {
            let was_there = d::quitar(&db, &project, &next_to_dir(std::path::Path::new(&folder)).to_string_lossy())?;
            if c.json {
                return json(&serde_json::json!({ "removed": was_there }));
            }
            if was_there {
                println!("{}", tr!("removed {folder}", "quitada {folder}"));
            } else {
                println!("{}", tr!("{folder} is not a documentation folder of this project (see `omoi docs list`)", "{folder} no es una carpeta de documentación de este proyecto (mira `omoi docs list`)"));
            }
            Ok(())
        }
        DocsAction::Refresh => {
            let Some(refreshed) = d::refrescar_proyecto(&db, &project)? else {
                // Another refresh of the project (the one at a session start, or one by hand) hasn't finished yet.
                let busy = tr!(
                    "the docs of this project are already being refreshed (try again in a few minutes)",
                    "ya se está poniendo al día la documentación de este proyecto (prueba en unos minutos)"
                );
                if c.json {
                    eprintln!("{busy}");
                    return json(&Vec::<serde_json::Value>::new());
                }
                println!("{busy}");
                return Ok(());
            };
            if c.json {
                return json(&refreshed.iter().map(|(folder, r)| serde_json::json!({ "folder": folder, "refresh": r })).collect::<Vec<_>>());
            }
            for (folder, r) in &refreshed {
                println!("{}", tr!("{folder}: {} indexed, {} removed, {} unchanged", "{folder}: {} indexados, {} quitados, {} sin cambios", r.indexed, r.removed, r.unchanged));
            }
            Ok(())
        }
        DocsAction::Test { texto: text } => {
            let e = d::buscar(&db, &project, &text, &Default::default())?;
            if c.json {
                return json(&e);
            }
            match e {
                Some(e) => println!("{} › {}  ({} {:.2})\n{}", e.path, e.title, tr!("coverage", "cobertura"), e.coverage, e.excerpt),
                None => println!("{}", tr!("nothing: recall would bring no documentation for this", "nada: el recall no traería documentación con esto")),
            }
            Ok(())
        }
    }
}

fn requested_scope(c: &Common, db: &Connection, all: bool, project: Option<&str>, space: Option<&str>) -> Result<Scope> {
    let current = project_of(c)?;
    // Its trades, if they were never looked at: without them it wouldn't see the memories of its trade. Optional.
    if let Err(e) = omoikane::oficios::preparar(db, &current) {
        eprintln!("omoikane: trades: {e:#}");
    }
    let base = Scope::from_project(db, &current)?;
    let requested = if let Some(s) = space {
        Scope::Space(Some(proyecto::buscar_espacio(db, s)?))
    } else if let Some(p) = project {
        let id = proyecto::buscar_en(db, p, &base)?;
        Scope::Project { id, space: proyecto::espacio_de(db, id)? }
    } else if all {
        base.widen()
    } else {
        base
    };
    anyhow::ensure!(
        base.allows(&requested),
        tr!(
            "that is in another space: from this project you only see its own space; run it from that project or from your home folder",
            "eso es de otro espacio: desde este proyecto solo se ve su espacio; hazlo desde ese proyecto o desde tu carpeta de usuario"
        )
    );
    Ok(requested)
}

/// A paused project with no space here goes into one (`projects move --space`, `space assign`) only with
/// `--force`: the pause was set by another computer saying it belongs to a client, and accepting it sends its
/// memories to that client's repository.
fn paused_without_space_needs_force(db: &Connection, id: i64, target: i64, paused: bool, force: bool) -> Result<()> {
    if paused && !force && proyecto::espacio_de(db, id)?.is_none() {
        bail!(tr!(
            "the project is paused because another computer says it belongs to a space: moving it to «{}» sends its memories to that space's sync repository. If you are not sure, `omoi projects keep` leaves it as it is; add --force if moving it is what you want",
            "el proyecto está en pausa porque otro equipo dice que es de un espacio: pasarlo a «{}» manda sus memorias al repositorio de ese espacio. Si no lo sabes, `omoi projects keep` lo deja como está; añade --force si es lo que quieres",
            proyecto::clave_espacio(db, Some(target))?
        ));
    }
    Ok(())
}

/// `omoi projects`.
fn projects_cli(c: &Common, action: ProjectsAction) -> Result<()> {
    let mut db = open_db(c)?;
    match action {
        ProjectsAction::List => {
            let base = Scope::from_project(&db, &project_of(c)?)?;
            let filter = if matches!(base, Scope::All) { None } else { Some(base.space()) };
            let mut v = proyecto::lista(&db, None, filter, usize::MAX)?;
            // Their trades: for those never looked at, now (the root and one level of each).
            for p in v.iter_mut().filter(|p| !p.gone && p.trades.is_empty()) {
                p.trades = omoikane::oficios::al_dia_si_falta(&db, p.id, &p.path)?;
            }
            if c.json {
                return json(&v);
            }
            for p in &v {
                let space_name = if p.space.is_empty() { String::new() } else { format!(" [{}]", p.space) };
                let ido = if p.gone { tr!("  (folder gone)", "  (ya no está)") } else { String::new() };
                let trade_names = if p.trades.is_empty() { String::new() } else { format!("  ({})", p.trades.join(", ")) };
                println!("{:>5}  {}  {}{space_name}  {}{ido}{trade_names}", p.memories, p.last_update.get(..10).unwrap_or(""), p.name, p.path);
            }
            Ok(())
        }
        ProjectsAction::Move { proyecto: p, carpeta: folder, space, force } => {
            let id = proyecto::buscar(&db, &p)?;
            // From one space to another, only with --force (like merge and move by folder): a client's things
            // would go to another's repository. From none to one, it's not needed, unless it is paused
            // (because another computer says it belongs to a space: it may not). The space named by the line
            // that paused the project is created here if it isn't yet.
            let key = proyecto::clave_de(&db, id)?;
            let pause = proyecto::en_pausa(&db)?.into_iter().find(|x| x.key == key);
            let target = match space.as_deref() {
                Some(s) if pause.as_ref().is_some_and(|x| !x.pausa.space.is_empty() && x.pausa.space == proyecto::clave_de_espacio(s)) => {
                    proyecto::espacio_de_clave(&db, &proyecto::clave_de_espacio(s))?
                }
                Some(s) => Some(proyecto::buscar_espacio(&db, s)?),
                None => None,
            };
            if let Some(d) = target {
                paused_without_space_needs_force(&db, id, d, pause.is_some(), force)?;
            }
            if target.is_some() && proyecto::espacio_de(&db, id)?.is_none() && made_personal(&db, id)? && !force {
                bail!(tr!(
                    "this project was made personal on purpose (`omoi space unassign --force`): add --force to put it in a space",
                    "este proyecto se hizo personal a propósito (`omoi space unassign --force`): añade --force para meterlo en un espacio"
                ));
            }
            if let (Some(before), Some(d)) = (proyecto::espacio_de(&db, id)?, target)
                && before != d
                && !force
            {
                bail!(tr!(
                    "the project is in the space «{}»: moving it to «{}» sends its memories to that space's sync repository. Add --force if that is what you want",
                    "el proyecto está en el espacio «{}»: moverlo a «{}» manda sus memorias al repositorio de ese espacio. Añade --force si es lo que quieres",
                    proyecto::clave_espacio(&db, Some(before))?,
                    proyecto::clave_espacio(&db, Some(d))?
                ));
            }
            if let Some(folder) = folder {
                let new_path = proyecto::de(&folder)?;
                let merged = proyecto::mover(&mut db, id, &new_path, force)?;
                if merged > 0 {
                    println!("{}", tr!("{new_path}: moved there (its {merged} memories joined it)", "{new_path}: movido allí (se le han sumado sus {merged} memorias)"));
                } else {
                    println!("{}", tr!("moved to {new_path}", "movido a {new_path}"));
                }
            }
            if let (Some(s), Some(d)) = (space, target) {
                proyecto::asignar_espacio(&db, id, Some(d))?;
                db.execute("DELETE FROM meta WHERE k = 'personal:' || ?1", [id])?;
                println!("{}", tr!("in the space «{s}» now", "ahora en el espacio «{s}»"));
                if pause.is_some() {
                    println!("{}", tr!("it is no longer paused: it syncs again", "ya no está en pausa: vuelve a sincronizarse"));
                }
            }
            Ok(())
        }
        ProjectsAction::Keep { proyecto: p } => {
            let id = proyecto::buscar(&db, &p)?;
            match proyecto::quedarse(&db, id)? {
                None => println!("{}", tr!("it is not paused: nothing to do", "no está en pausa: nada que hacer")),
                Some(e) if e.is_empty() => println!("{}", tr!("it stays in no space and syncs again", "se queda sin espacio y vuelve a sincronizarse")),
                Some(e) => println!("{}", tr!("it stays in the space «{e}» and syncs again", "se queda en el espacio «{e}» y vuelve a sincronizarse")),
            }
            omoikane::sync::programar(&db_path(c));
            Ok(())
        }
        ProjectsAction::Prune => {
            let n = proyecto::podar(&db)?;
            println!("{}", tr!("{n} projects without memories forgotten", "{n} proyectos sin memorias olvidados"));
            Ok(())
        }
        ProjectsAction::Distinct { a, b } => {
            let from = proyecto::buscar(&db, &a)?;
            let the_other = b.as_deref().map(|b| proyecto::buscar(&db, b)).transpose()?;
            omoikane::nombres::mark_distinct(&db, from, the_other)?;
            println!("{}", tr!("ok: the review won't propose it again", "hecho: la revisión no volverá a proponerlo"));
            Ok(())
        }
        ProjectsAction::Merge { a, b, force } => {
            let (from, to) = (proyecto::buscar(&db, &a)?, proyecto::buscar(&db, &b)?);
            let n = proyecto::fundir(&mut db, from, to, force)?;
            println!("{}", tr!("merged: {n} memories moved to {b}", "fundidos: {n} memorias pasan a {b}"));
            Ok(())
        }
    }
}

/// `omoi space`.
fn spaces_cli(c: &Common, action: SpaceAction) -> Result<()> {
    let db = open_db(c)?;
    let chosen = |p: Option<String>| -> Result<i64> {
        match p {
            Some(p) => proyecto::buscar(&db, &p),
            None => proyecto::registrar(&db, &project_of(c)?),
        }
    };
    match action {
        SpaceAction::List => {
            let v = proyecto::espacios(&db)?;
            if c.json {
                return json(&v);
            }
            if v.is_empty() {
                println!("{}", tr!("no spaces yet: `omoi space add <name>`", "todavía no hay espacios: `omoi space add <nombre>`"));
            }
            for e in &v {
                println!("{}  ({} {})  {}", e.key, e.memories, tr!("memories", "memorias"), e.projects.join(", "));
            }
            Ok(())
        }
        SpaceAction::Add { nombre: name } => {
            proyecto::crear_espacio(&db, &name)?;
            println!("{}", tr!("space «{}» created; put projects in it with `omoi space assign {}`", "espacio «{}» creado; mete proyectos con `omoi space assign {}`", proyecto::clave_de_espacio(&name), proyecto::clave_de_espacio(&name)));
            Ok(())
        }
        SpaceAction::Assign { espacio: space_name, proyecto: p, force } => {
            let s = proyecto::buscar_espacio(&db, &space_name)?;
            let id = chosen(p)?;
            let paused = proyecto::pausado(&db, &proyecto::clave_de(&db, id)?)?;
            paused_without_space_needs_force(&db, id, s, paused, force)?;
            // From one space to another, only with --force, like `projects move --space`: a client's things
            // would go to another's repository.
            if let Some(before) = proyecto::espacio_de(&db, id)?
                && before != s
                && !force
            {
                bail!(tr!(
                    "the project is in the space «{}»: moving it to «{}» sends its memories to that space's sync repository. Add --force if that is what you want",
                    "el proyecto está en el espacio «{}»: moverlo a «{}» manda sus memorias al repositorio de ese espacio. Añade --force si es lo que quieres",
                    proyecto::clave_espacio(&db, Some(before))?,
                    proyecto::clave_espacio(&db, Some(s))?
                ));
            }
            if made_personal(&db, id)? && !force {
                bail!(tr!(
                    "this project was made personal on purpose (`omoi space unassign --force`): add --force to put it in «{space_name}»",
                    "este proyecto se hizo personal a propósito (`omoi space unassign --force`): añade --force para meterlo en «{space_name}»"
                ));
            }
            proyecto::asignar_espacio(&db, id, Some(s))?;
            db.execute("DELETE FROM meta WHERE k = 'personal:' || ?1", [id])?;
            println!("{}", tr!("done: its memories are now seen only from the space «{space_name}» (and the home folder)", "hecho: sus memorias ya solo se ven desde el espacio «{space_name}» (y desde la carpeta de usuario)"));
            Ok(())
        }
        SpaceAction::Unassign { proyecto: p, force } => {
            let id = chosen(p)?;
            // Taking it out of its space sends its memories to the general repository, the personal one: a
            // client's things would end up there. Only with --force.
            if let Some(before) = proyecto::espacio_de(&db, id)?
                && !force
            {
                bail!(tr!(
                    "the project is in the space «{}»: without a space, its memories go to the general (personal) sync repository. Add --force if that is what you want",
                    "el proyecto está en el espacio «{}»: sin espacio, sus memorias van al repositorio general (el personal). Añade --force si es lo que quieres",
                    proyecto::clave_espacio(&db, Some(before))?
                ));
            }
            // What came from other computers through the space's repository belongs to the client, not to this
            // person: it stays in the space, as a space memory (it still goes only to its
            // repository and is no longer seen from this project). What was written here leaves with the project.
            let mut kept_in_space = 0;
            if let Some(before) = proyecto::espacio_de(&db, id)? {
                kept_in_space = db.execute(
                    "UPDATE memory SET project_id = 0, space_id = ?2 WHERE project_id = ?1 AND source LIKE 'sync:%'",
                    rusqlite::params![id, before],
                )?;
            }
            proyecto::asignar_espacio(&db, id, None)?;
            // The person has decided it is personal: putting it in a space again will require --force,
            // even if a client's repository keeps proposing it.
            db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('personal:' || ?1, '1')", [id])?;
            println!("{}", tr!("done: the project is no longer in a space", "hecho: el proyecto ya no está en ningún espacio"));
            if kept_in_space > 0 {
                println!(
                    "{}",
                    tr!(
                        "{kept_in_space} memories that came from other computers through the space's repository stay in the space (they are the client's)",
                        "{kept_in_space} memorias que llegaron de otros equipos por el repositorio del espacio se quedan en el espacio (son del cliente)"
                    )
                );
            }
            Ok(())
        }
    }
}

/// The trades of `remember` (`--trade`, or `trade:` tags set by hand), validated and without
/// repeats, and the tags without the trade ones.
fn requested_trades(trades: &[String], tags: Vec<String>) -> Result<(Vec<String>, Vec<String>)> {
    let mut v: Vec<String> = Vec::new();
    let by_hand: Vec<String> = tags.iter().filter_map(|t| t.trim().to_lowercase().strip_prefix(omoikane::oficios::PREFIJO).map(String::from)).collect();
    for t in trades.iter().chain(&by_hand) {
        for o in omoikane::oficios::lista(t)? {
            if !v.contains(&o) {
                v.push(o);
            }
        }
    }
    let tags = tags.into_iter().filter(|t| !t.trim().to_lowercase().starts_with(omoikane::oficios::PREFIJO)).collect();
    Ok((v, tags))
}

/// If a trade memory names something of a project or a client, it says so and exits with code 2
/// (like a duplicate): it is not saved (except the false positives `--allow-leaks` lets through).
fn warn_leaks(c: &Common, leaks: &[omoikane::oficios::Fuga]) -> Result<()> {
    if leaks.is_empty() {
        return Ok(());
    }
    if c.json {
        json(&serde_json::json!({ "saved": false, "trade_leaks": leaks }))?;
    } else {
        println!(
            "{}",
            tr!(
                "not saved: a trade memory reaches every project of that trade, of every client, and this one names what looks like a project or a client:",
                "no guardada: una memoria de oficio llega a todos los proyectos de ese oficio, de todos los clientes, y esta nombra lo que parece de un proyecto o de un cliente:"
            )
        );
        for f in leaks {
            if f.campo.is_empty() {
                println!("  {:<10}  {}", f.tipo, f.text);
            } else {
                println!("  {:<10}  {}  {}", f.tipo, f.text, tr!("(in its {})", "(en su {})", f.campo));
            }
        }
        println!(
            "{}",
            tr!(
                "  rewrite it without them, or save it in the project (without --trade); if you checked they are false positives, add --allow-leaks (it never lets through an absolute path or a repository URL)",
                "  reescríbela sin eso, o guárdala en el proyecto (sin --trade); si has comprobado que son falsos positivos, añade --allow-leaks (nunca deja pasar una ruta absoluta ni la URL de un repositorio)"
            )
        );
    }
    std::process::exit(2);
}

/// The trades of `update` (see `oficios::al_cambiar`): `--trade` makes it a trade memory (promotes it),
/// `--no-trade` makes it stop being one, `--for` says who it is for; and what is new in a trade memory (text,
/// paths, tags, data) goes through `oficios::fugas` (except what `--allow-leaks` lets through).
/// Returns the changes and, if it is a client's and gets promoted, the client's memory: then it is not
/// changed, a new trade memory is created (`oficios::crear_de_oficio`).
#[allow(clippy::too_many_arguments)]
fn trades_for_update(
    c: &Common,
    db: &Connection,
    id: &str,
    mut changes: Changes,
    trades: &[String],
    no_trade: bool,
    audience: Option<&str>,
    data: Option<&Option<serde_json::Value>>,
    allow_leaks: bool,
) -> Result<(Changes, Option<Memory>)> {
    use omoikane::oficios;
    let current = memoria::get(db, id)?;
    let trade = if no_trade { Some(String::new()) } else { (!trades.is_empty()).then(|| trades.join(",")) };
    let requested = oficios::PedidoCambio { trade: trade.as_deref(), para: audience, cambios: &changes, datos: data.map(|d| d.as_ref()), permitir_fugas: allow_leaks };
    let plan = match oficios::al_cambiar(db, &current, requested)? {
        Ok(plan) => plan,
        Err(leaks) => {
            warn_leaks(c, &leaks)?;
            unreachable!()
        }
    };
    changes.tags = plan.tags;
    if plan.nueva {
        return Ok((changes, Some(current)));
    }
    if plan.hacer_global {
        changes.project = Some(String::new());
    }
    Ok((changes, None))
}

/// `omoi trades`.
fn trades_cli(c: &Common, action: TradesAction) -> Result<()> {
    use omoikane::oficios;
    let db = open_db(c)?;
    // The requested project (or this folder's, noting it down if it isn't yet) and its folder.
    let chosen = |p: Option<String>| -> Result<(i64, String)> {
        match p {
            Some(p) => {
                let id = proyecto::buscar(&db, &p)?;
                let path: String = db.query_row("SELECT path FROM project WHERE id = ?1", [id], |f| f.get(0))?;
                Ok((id, path))
            }
            None => {
                let path = project_of(c)?;
                anyhow::ensure!(
                    !proyecto::es_general(&path),
                    tr!("this folder is not a project: run it in one, or give --project", "esta carpeta no es un proyecto: hazlo en uno, o di --project")
                );
                Ok((proyecto::registrar(&db, &path)?, path))
            }
        }
    };
    let names = |v: &[String]| -> Result<Vec<String>> { v.iter().map(|o| oficios::nombre(o)).collect() };
    let show_state = |id: i64| -> Result<()> {
        let e = oficios::estado(&db, id)?;
        if c.json {
            return json(&e);
        }
        let o = if e.trades.is_empty() { tr!("(none)", "(ninguno)") } else { e.trades.join(", ") };
        println!("{}", tr!("trades: {o}", "oficios: {o}"));
        if !e.detected.is_empty() {
            println!("{}", tr!("  detected from its files: {}", "  detectados en sus ficheros: {}", e.detected.join(", ")));
        }
        if !e.added.is_empty() {
            println!("{}", tr!("  added by hand: {}", "  puestos a mano: {}", e.added.join(", ")));
        }
        if !e.removed.is_empty() {
            println!("{}", tr!("  removed by hand: {}", "  quitados a mano: {}", e.removed.join(", ")));
        }
        if e.fixed {
            println!("{}", tr!("  fixed with `omoi trades set`: detection adds nothing (`omoi trades auto` undoes it)", "  fijados con `omoi trades set`: la detección no añade nada (`omoi trades auto` lo deshace)"));
        }
        Ok(())
    };
    match action {
        TradesAction::List { all: false } => {
            let (id, path) = chosen(None)?;
            oficios::al_dia(&db, id, &path)?;
            show_state(id)
        }
        TradesAction::List { all: true } => {
            let base = Scope::from_project(&db, &project_of(c)?)?;
            let filter = if matches!(base, Scope::All) { None } else { Some(base.space()) };
            let mut v = proyecto::lista(&db, None, filter, usize::MAX)?;
            for p in v.iter_mut().filter(|p| !p.gone) {
                p.trades = oficios::al_dia_si_falta(&db, p.id, &p.path)?;
            }
            let by_trade = oficios::memorias_por_oficio(&db)?;
            if c.json {
                let projects: Vec<_> = v.iter().map(|p| serde_json::json!({ "name": p.name, "path": p.path, "trades": p.trades })).collect();
                return json(&serde_json::json!({ "projects": projects, "memories": by_trade }));
            }
            for p in &v {
                println!("{:<28}  {}", p.name, if p.trades.is_empty() { "-".to_string() } else { p.trades.join(", ") });
            }
            if !by_trade.is_empty() {
                let mut counts: Vec<_> = by_trade.into_iter().collect();
                counts.sort();
                println!("\n{}", tr!("trade memories: {}", "memorias de oficio: {}", counts.iter().map(|(o, n)| format!("{o} {n}")).collect::<Vec<_>>().join(", ")));
            }
            Ok(())
        }
        TradesAction::Add { oficios: os, project } => {
            let (id, _) = chosen(project)?;
            oficios::anadir(&db, id, &names(&os)?)?;
            show_state(id)
        }
        TradesAction::Remove { oficios: os, project } => {
            let (id, _) = chosen(project)?;
            oficios::quitar(&db, id, &names(&os)?)?;
            show_state(id)
        }
        TradesAction::Set { oficios: os, project } => {
            let (id, _) = chosen(project)?;
            oficios::fijar(&db, id, &names(&os)?)?;
            show_state(id)
        }
        TradesAction::Auto { project } => {
            let (id, path) = chosen(project)?;
            oficios::automatico(&db, id, &path)?;
            show_state(id)
        }
    }
}

/// Whether the person took the project out of its space on purpose (`space unassign --force`).
fn made_personal(db: &Connection, id: i64) -> Result<bool> {
    Ok(db.query_row("SELECT count(*) FROM meta WHERE k = 'personal:' || ?1", [id], |f| f.get::<_, i64>(0))? > 0)
}

fn text_or_stdin(t: Option<String>) -> Result<String> {
    match t.as_deref() {
        Some(t) if t != "-" => Ok(t.to_string()),
        _ => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .with_context(|| tr!("can't read standard input", "no se puede leer la entrada estándar"))?;
            if s.trim().is_empty() {
                bail!(tr!(
                    "missing content: pass it as an argument or through standard input",
                    "falta el contenido: pásalo como argumento o por la entrada estándar"
                ));
            }
            Ok(s)
        }
    }
}

/// On stderr (also with `--json`): what was saved is not what was written.
/// If the change removed a person's verification: let whoever changes it know.
fn warn_unverified(m: &Memory) {
    if let Some(v) = &m.unverified {
        eprintln!(
            "{}",
            tr!(
                "its text changed, so it is no longer verified (it was by {}): `omoi verify {}` if it is still right",
                "su texto ha cambiado, así que ya no está verificada (lo estaba por {}): `omoi verify {}` si sigue siendo correcta",
                v.by,
                short_id(&m.id)
            )
        );
    }
}

/// `omoi verify`: only a person. In a terminal it asks; elsewhere, it requires `--yes`; inside an agent's
/// shell it always refuses (see `verificado`).
fn verify_cli(c: &Common, v: VerifyArgs) -> Result<()> {
    if let Some(var) = omoikane::verificado::shell_de_agente() {
        bail!(tr!(
            "verifying is for a person, and this looks like a coding agent's shell ({var} is set): run it in your own terminal, or use the web view (`omoi serve --open`)",
            "verificar es cosa de una persona, y esto parece la shell de un agente ({var} está puesta): hazlo en tu terminal, o en la vista web (`omoi serve --open`)"
        ));
    }
    let mut db = open_db(c)?;
    let m = memoria::get(&db, &v.id)?;
    if v.unverify {
        let was_verified = omoikane::verificado::desverificar(&mut db, &m.id)?;
        if c.json {
            return json(&serde_json::json!({ "id": m.id, "verified": false, "changed": was_verified }));
        }
        println!("{}", if was_verified { tr!("{} is no longer verified", "{} ya no está verificada", short_id(&m.id)) } else { tr!("{} was not verified", "{} no estaba verificada", short_id(&m.id)) });
        return Ok(());
    }
    let by = match v.by {
        Some(b) => b,
        None => omoikane::verificado::quien_por_defecto(&db, &m)?,
    };
    if !v.yes {
        if !omoikane::entorno::puede_preguntar() {
            bail!(tr!("add --yes to verify {} without asking", "añade --yes para verificar {} sin preguntar", short_id(&m.id)));
        }
        println!("{}  {}\n{}\n\n{}\n", short_id(&m.id), m.kind, m.title, m.content);
        let who = if by.trim().is_empty() { tr!("(no name: give --by)", "(sin nombre: di --by)") } else { by.clone() };
        if !ask_yes_no(&tr!("Verify it as {who}? You vouch that this text is right.", "¿Verificarla como {who}? Respondes de que este texto es correcto."), false)? {
            println!("{}", tr!("nothing changed", "no se ha cambiado nada"));
            return Ok(());
        }
    }
    let m = omoikane::verificado::verificar(&mut db, &m.id, &by)?;
    if c.json {
        return json(&m);
    }
    let who = m.verified.as_ref().map(|x| x.by.clone()).unwrap_or(by);
    println!("{}", tr!("verified {} by {who}", "verificada {} por {who}", short_id(&m.id)));
    Ok(())
}

fn warn_redacted(m: &Memory) {
    if !m.redacted.is_empty() {
        let r = omoikane::secretos::resumen(&m.redacted);
        eprintln!(
            "{}",
            tr!(
                "redacted {r}: what looked like a secret was saved as [redacted:<type>] (OMOIKANE_REDACT=0 turns this off)",
                "redactado {r}: lo que parecía un secreto se ha guardado como [redacted:<tipo>] (OMOIKANE_REDACT=0 lo apaga)"
            )
        );
    }
}

/// Whether an `update` changes nothing of the memory (only, perhaps, its data).
fn no_changes(c: &Changes) -> bool {
    c.title.is_none()
        && c.content.is_none()
        && c.kind.is_none()
        && c.paths.is_none()
        && c.tags.is_none()
        && c.pinned.is_none()
        && c.project.is_none()
        && c.space_id.is_none()
        && c.replace.is_empty()
        && c.append.is_none()
}

/// What was masked in the data for looking like a secret, to stderr.
fn warn_masked_data(kinds: &[&str]) {
    if !kinds.is_empty() {
        eprintln!(
            "{}",
            tr!(
                "omoikane: what looked like a secret in the data was replaced by [redacted:<type>] ({})",
                "omoikane: lo que parecía un secreto en los datos se cambió por [redacted:<tipo>] ({})",
                kinds.join(", ")
            )
        );
    }
}

fn json(v: &impl serde::Serialize) -> Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut output, v)?;
    writeln!(output)?;
    Ok(())
}

/// What the CLI shows of the id: 10 characters of time and 2 random ones. `get` accepts this prefix.
fn short_id(id: &str) -> &str {
    id.char_indices().nth(12).map_or(id, |(i, _)| &id[..i])
}

fn one_line(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max { s } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

fn listing(m: &Memory, with_project: bool) {
    let pinned = if m.pinned { tr!(" (pinned)", " (fijada)") } else { String::new() };
    let verified = m.verified.as_ref().map(|v| tr!(" (verified by {})", " (verificada por {})", v.by)).unwrap_or_default();
    println!("{}  {:<10} {}{pinned}{verified}", short_id(&m.id), m.kind.as_str(), m.title);
    let mut extra: Vec<String> = m.paths.clone();
    extra.extend(m.tags.iter().filter(|t| !t.starts_with(omoikane::oficios::PREFIJO) && !t.starts_with(omoikane::oficios::PARA)).map(|t| format!("#{t}")));
    let trade_names = omoikane::oficios::de_memoria(m);
    if !trade_names.is_empty() {
        let audience = omoikane::oficios::para_de(&m.tags).map(|p| format!(" ({p})")).unwrap_or_default();
        extra.push(format!("trade {}{audience}", trade_names.join(", ")));
    } else if m.project.is_empty() {
        extra.push("global".into());
    } else if with_project {
        extra.push(m.project.clone());
    }
    if !extra.is_empty() {
        println!("              {}", extra.join(" · "));
    }
    println!("              {}", one_line(&m.content, 160));
}

fn print_full(m: &Memory, applied: Option<(i64, Option<String>)>) {
    println!("{}  {}{}", m.id, m.kind, if m.pinned { tr!("  (pinned)", "  (fijada)") } else { String::new() });
    println!("{}\n", m.title);
    println!("{}\n", m.content);
    if !m.paths.is_empty() {
        println!("{}{}", tr!("paths      ", "rutas      "), m.paths.join(", "));
    }
    if !m.tags.is_empty() {
        let tags = m.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ");
        println!("{}{tags}", tr!("tags       ", "etiquetas  "));
    }
    let trade_names = omoikane::oficios::de_memoria(m);
    let project = if !trade_names.is_empty() {
        format!("trade {}", trade_names.join(", "))
    } else if m.project.is_empty() {
        "global".to_string()
    } else {
        m.project.clone()
    };
    println!("{}{project}", tr!("project    ", "proyecto   "));
    println!("{}{}", tr!("source     ", "origen     "), m.source);
    if let Some(p) = &m.provenance {
        println!("{}{}", tr!("written in ", "escrita en "), p.text());
    }
    for (i, e) in m.evidence.iter().enumerate() {
        let label = if i == 0 { tr!("based on   ", "se basa en ") } else { "           ".to_string() };
        println!("{label}{}", e.text());
    }
    if let Some(f) = omoikane::revisar::review_date_of(&m.tags) {
        match &m.review_reason {
            Some(r) => println!("{}", tr!("review     {f}: {r}", "revisar    {f}: {r}")),
            None => println!("{}", tr!("review     {f}", "revisar    {f}")),
        }
    }
    match &m.verified {
        Some(v) => println!("{}", tr!("verified   by {} ({})", "verificada por {} ({})", v.by, v.at)),
        None => println!("{}", tr!("verified   no: written by an agent, or changed since", "verificada no: la escribió un agente, o cambió después")),
    }
    println!("{}", tr!("created    {}   changed {}", "creada     {}   cambiada {}", m.created_at, m.updated_at));
    if m.recall_count > 0 {
        let last = m.recalled_at.as_deref().unwrap_or("-");
        let n = m.recall_count;
        let times = if n == 1 { tr!("once", "1 vez") } else { tr!("{n} times", "{n} veces") };
        println!("{}", tr!("recalled   {times} (last {last})", "recuperada {times} (la última, {last})"));
    }
    if let Some((n, Some(last))) = applied.filter(|(n, _)| *n > 0) {
        let times = if n == 1 { tr!("in 1 session", "en 1 sesión") } else { tr!("in {n} sessions", "en {n} sesiones") };
        println!("{}", tr!("applied    {times} (last {last})", "aplicada   {times} (la última, {last})"));
    }
}

fn print_history(m: &Memory, v: &[Version]) {
    println!("{}  {}", short_id(&m.id), m.title);
    if let Some(p) = &m.provenance {
        println!("{}", tr!("written in {} ({})", "escrita en {} ({})", p.text(), m.created_at));
    }
    if let Some(x) = &m.verified {
        println!("{}", tr!("verified by {} ({})", "verificada por {} ({})", x.by, x.at));
    }
    if v.is_empty() {
        println!("{}", tr!("no previous versions", "sin versiones anteriores"));
        return;
    }
    for x in v {
        let reason_text = x.reason.as_deref().map(|r| format!(" · {r}")).unwrap_or_default();
        println!("\n{}", tr!("v{}  until {}{reason_text}", "v{}  hasta {}{reason_text}", x.version, x.changed_at));
        println!("  {}  {}", x.kind, x.title);
        println!("  {}", one_line(&x.content, 300));
    }
}

/// «indexing RAM: 64 MB (default)…», for `omoi model semantic` and `omoi doctor`.
fn ram_in_use(mb: u64, origin: omoikane::significado::OrigenRam) -> String {
    use omoikane::significado::{OrigenRam, RAM_MINIMA_MB};
    let from = match origin {
        OrigenRam::Entorno => tr!("from OMOIKANE_SEMANTIC_RAM", "de OMOIKANE_SEMANTIC_RAM"),
        OrigenRam::Elegido => tr!("chosen with `omoi model semantic --ram`", "elegido con `omoi model semantic --ram`"),
        OrigenRam::PorDefecto => tr!("default; `omoi model semantic --ram <MB>` changes it", "por defecto; `omoi model semantic --ram <MB>` lo cambia"),
    };
    tr!("indexing RAM: up to {mb} MB ({from}; minimum {RAM_MINIMA_MB})", "RAM al indexar: hasta {mb} MB ({from}; mínimo {RAM_MINIMA_MB})")
}

fn doctor(c: &Common) -> Result<()> {
    db::registrar_sqlite_vec();
    let probe = Connection::open_in_memory()?;
    let sqlite: String = probe.query_row("select sqlite_version()", [], |r| r.get(0))?;
    let vec: String = probe.query_row("select vec_version()", [], |r| r.get(0))?;
    probe.execute_batch("create virtual table t using fts5(x, tokenize='unicode61 remove_diacritics 2');")?;
    probe.execute_batch("create virtual table v using vec0(e float[4]);")?;
    println!("sqlite     {sqlite}");
    println!("sqlite-vec {vec}");
    println!("{}", tr!("fts5       ok (unicode61, accent-insensitive)", "fts5       ok (unicode61, sin acentos)"));
    println!("vec0       ok");
    let path = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
    if path.exists() {
        let base = db::abrir(&path)?;
        let n: i64 = base.query_row("SELECT count(*) FROM memory", [], |f| f.get(0))?;
        let p: i64 = base.query_row("SELECT count(DISTINCT project_id) FROM memory WHERE project_id != 0", [], |f| f.get(0))?;
        let v = db::VERSION_ESQUEMA;
        let r = path.display();
        let memories = if n == 1 { tr!("1 memory", "1 memoria") } else { tr!("{n} memories", "{n} memorias") };
        let projects = if p == 1 { tr!("1 project", "1 proyecto") } else { tr!("{p} projects", "{p} proyectos") };
        println!("{}", tr!("database   {r} ({memories}, {projects}, schema {v})", "base       {r} ({memories}, {projects}, esquema {v})"));
        for x in proyecto::lista(&base, None, None, usize::MAX)?.into_iter().filter(|x| x.gone) {
            println!("{}", tr!("project    {} is gone from {}: `omoi projects move {} <new folder>`", "proyecto   {} ya no está en {}: `omoi projects move {} <carpeta nueva>`", x.name, x.path, x.name));
        }
        for a in omoikane::sync::avisos_de_pausa(&base, &path) {
            println!("{}{a}", tr!("paused     ", "en pausa   "));
        }
        let trash: i64 = base.query_row("SELECT count(*) FROM forgotten", [], |f| f.get(0))?;
        let copies = omoikane::copia::copias(&path);
        let total_kb: u64 = copies.iter().map(|(_, b)| b).sum::<u64>() / 1024;
        match copies.first() {
            Some((p, _)) => {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                println!("{}", tr!("backups    {} ({} copies, {total_kb} KB, {} in the trash)", "copias     {} ({} copias, {total_kb} KB, {} en la papelera)", name, copies.len(), trash));
            }
            None if omoikane::copia::copias_a_conservar() == 0 => println!("{}", tr!("backups    off (OMOIKANE_BACKUPS=0)", "copias     apagadas (OMOIKANE_BACKUPS=0)")),
            None => println!("{}", tr!("backups    none yet (the first one happens at the next session start)", "copias     todavía ninguna (la primera, al empezar la próxima sesión)")),
        }
        let with_secret = memoria::recent(&base, &Scope::All, None, usize::MAX >> 1)?
            .iter()
            .filter(|m| !omoikane::revisar::secrets_of(m).is_empty())
            .count();
        let redaction = if omoikane::secretos::activo() {
            tr!("on", "activa")
        } else {
            tr!("OFF (OMOIKANE_REDACT=0): secrets are saved as written", "APAGADA (OMOIKANE_REDACT=0): los secretos se guardan tal cual")
        };
        match with_secret {
            0 => println!("{}", tr!("secrets    redaction {redaction}; no memory looks like it holds one", "secretos   redacción {redaction}; ninguna memoria parece llevar uno")),
            1 => println!(
                "{}",
                tr!(
                    "secrets    redaction {redaction}; 1 memory looks like it holds one (`omoi review`, or Redact in the web view)",
                    "secretos   redacción {redaction}; 1 memoria parece llevar uno (`omoi review`, o Redactar en la vista web)"
                )
            ),
            n => println!(
                "{}",
                tr!(
                    "secrets    redaction {redaction}; {n} memories look like they hold one (`omoi review`, or Redact in the web view)",
                    "secretos   redacción {redaction}; {n} memorias parecen llevar uno (`omoi review`, o Redactar en la vista web)"
                )
            ),
        }
    } else {
        let r = path.display();
        println!("{}", tr!("database   {r} (not created yet: it appears with the first memory)", "base       {r} (todavía no existe: se crea al guardar la primera memoria)"));
        if !omoikane::secretos::activo() {
            println!("{}", tr!("secrets    redaction OFF (OMOIKANE_REDACT=0): secrets are saved as written", "secretos   redacción APAGADA (OMOIKANE_REDACT=0): los secretos se guardan tal cual"));
        }
    }
    match modelos::actual() {
        Some(d) => {
            let name = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            println!("{}{name}", tr!("model      ", "modelo     "));
            if path.exists() {
                let (from, n) = vectores::estado(&db::abrir(&path)?)?;
                let up_to_date = from.as_deref() == Some(name.as_str());
                println!(
                    "{}",
                    if up_to_date {
                        tr!("vectors    {n}", "vectores   {n}")
                    } else {
                        tr!("vectors    {n} (out of date: they are recomputed on the next search)", "vectores   {n} (desfasados: se recalculan en la próxima búsqueda)")
                    }
                );
            }
        }
        None => println!(
            "{}",
            tr!("model      not installed (`omoi model install`): search by words only", "modelo     no instalado (`omoi model install`): búsqueda solo por palabras")
        ),
    }
    for (k, l) in meaning_report(&path)?.1.into_iter().enumerate() {
        println!("{}{l}", if k == 0 { tr!("meaning    ", "significado ") } else { "           ".to_string() });
    }
    if let Ok(p) = project_of(c) {
        println!("{}{p}", tr!("project    ", "proyecto   "));
    }
    let path = db_path(c);
    println!("{}{}", tr!("sync       ", "sincroniz. "), sync_status(&path, None));
    for e in omoikane::sync::espacios(&path) {
        println!("{}{}", tr!("  space {e}: ", "  espacio {e}: "), sync_status(&path, Some(&e)));
    }
    println!("{}", tr!("language   en (OMOIKANE_LANG=es for Spanish)", "idioma     es (OMOIKANE_LANG)"));
    Ok(())
}

/// A line with the sync state of a repository, the general one or a space's
/// (for `doctor` and `sync status`).
fn sync_status(path: &std::path::Path, space_name: Option<&str>) -> String {
    use omoikane::sync;
    let r = sync::rutas_de(path, space_name);
    let Ok(Some(config)) = sync::leer_config_de(&r) else {
        return tr!("off (`omoi sync init <git url>` to share memories between computers)", "apagada (`omoi sync init <url de git>` para compartir memorias entre equipos)");
    };
    let e = sync::leer_estado_de(&r);
    let ago = |s: &str| {
        let t = s.parse::<u64>().unwrap_or(0);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let m = now.saturating_sub(t) / 60;
        if m < 60 { tr!("{m} min ago", "hace {m} min") } else { tr!("{} h ago", "hace {} h", m / 60) }
    };
    let mut l = sync::sin_credenciales(&config.url);
    match (&e.ultimo_error, &e.ultimo_ok) {
        (Some(err), _) => l.push_str(&tr!(" · FAILING: {}", " · FALLA: {}", sync::sin_credenciales(err.lines().next().unwrap_or_default()))),
        (None, Some(ok)) => l.push_str(&tr!(" · last ok {}", " · última bien {}", ago(ok))),
        (None, None) => l.push_str(&tr!(" · not synced yet", " · aún sin sincronizar")),
    }
    if sync::pendiente(path) {
        l.push_str(&tr!(" · changes pending", " · cambios pendientes"));
    }
    if let Some(a) = e.aviso {
        l.push_str(&format!(" · {a}"));
    }
    if sync::clave_cambiada(&r) {
        l.push_str(&format!(" · {}", sync::aviso_clave_cambiada()));
    }
    // It says it even if last time's error already says it: it's what has to be done.
    if sync::necesita_version(&r).is_some() && !e.ultimo_error.as_deref().is_some_and(|x| x.contains(&sync::aviso_version())) {
        l.push_str(&format!(" · {}", sync::aviso_version()));
    }
    l
}

/// The space of a `--space` option, already checked.
fn checked_space(space: Option<String>) -> Result<Option<String>> {
    space.map(|e| omoikane::sync::espacio_valido(&e)).transpose()
}

/// A repository's keys for `sync status`: «k1, k2 (current k2)».
fn sync_keys(r: &omoikane::sync::Rutas) -> String {
    let (ids, current_key) = omoikane::sync::claves_de(r);
    let listing: Vec<String> = ids.iter().map(|i| format!("k{i}")).collect();
    let current_key = if current_key == 0 { ids.last().copied().unwrap_or(1) } else { current_key };
    tr!("{} (current: k{})", "{} (vigente: k{})", listing.join(", "), current_key)
}

fn ask_yes_no(text: &str, default_yes: bool) -> Result<bool> {
    let options = if default_yes { tr!("[Y/n]", "[S/n]") } else { tr!("[y/N]", "[s/N]") };
    print!("{text} {options} ");
    std::io::stdout().flush()?;
    let mut l = String::new();
    std::io::stdin().read_line(&mut l)?;
    let l = l.trim().to_lowercase();
    Ok(if l.is_empty() { default_yes } else { matches!(l.as_str(), "s" | "si" | "sí" | "y" | "yes") })
}

/// Shows what will be uploaded and, in a terminal, asks for confirmation. Without a terminal `--yes` is needed (an
/// agent that launches it must not upload memories without anyone having decided it).
/// `omoi sync init --create <name>`: creates the private repository with `gh` or, if it isn't there, with `glab`,
/// and returns its URL. It uses the session the user already has in that tool.
fn create_repository(name: &str) -> Result<String> {
    let has_tool = |cmd: &str| std::process::Command::new(cmd).arg("--version").output().is_ok_and(|o| o.status.success());
    let (cmd, args): (&str, Vec<&str>) = if has_tool("gh") {
        ("gh", vec!["repo", "create", name, "--private", "--description", "Omoikane: shared memory (encrypted)"])
    } else if has_tool("glab") {
        ("glab", vec!["repo", "create", name, "--private", "--description", "Omoikane: shared memory (encrypted)"])
    } else {
        bail!(tr!(
            "--create needs the GitHub (`gh`) or GitLab (`glab`) command line; create a private repository yourself and pass its URL",
            "--create necesita la línea de órdenes de GitHub (`gh`) o de GitLab (`glab`); crea tú un repositorio privado y pasa su URL"
        ));
    };
    let o = std::process::Command::new(cmd).args(&args).stdin(std::process::Stdio::null()).output()?;
    let text = format!("{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    if !o.status.success() {
        bail!("{cmd} repo create: {}", text.trim());
    }
    let url = text
        .split_whitespace()
        .find(|p| p.starts_with("https://"))
        .map(|u| format!("{}.git", u.trim_end_matches(['/', '.'])))
        .ok_or_else(|| anyhow::anyhow!("{cmd} repo create: no URL in its output: {}", text.trim()))?;
    eprintln!("{}", tr!("Created the private repository {url}", "Creado el repositorio privado {url}"));
    Ok(url)
}

fn confirm_upload(db: &Connection, config: &omoikane::sync::Config, e: &omoikane::sync::Eleccion, yes: bool) -> Result<bool> {
    let (g, p, np) = omoikane::sync::resumen_de_subida(db, config)?;
    println!(
        "{}",
        tr!(
            "Will upload {g} global memories and {p} from {np} project(s), encrypted (preferences: {}). Everything stays on this computer too.",
            "Se subirán {g} memorias globales y {p} de {np} proyecto(s), cifradas (preferencias: {}). Todo sigue también en este equipo.",
            if e.preferencias { tr!("yes", "sí") } else { "no".into() }
        )
    );
    if yes {
        return Ok(true);
    }
    anyhow::ensure!(
        omoikane::entorno::puede_preguntar(),
        tr!("add --yes to confirm (or run it in a terminal to choose what to share)", "añade --yes para confirmarlo (o ejecútalo en una terminal para elegir qué compartir)")
    );
    if !ask_yes_no(&tr!("Continue?", "¿Seguir?"), true)? {
        println!("{}", tr!("nothing changed", "no se ha cambiado nada"));
        return Ok(false);
    }
    Ok(true)
}

/// What to share: what the options say and, in a terminal without `--yes`, what the user answers.
fn choose(o: &SyncOptions) -> Result<omoikane::sync::Eleccion> {
    let projects = o.project.iter().map(|p| proyecto::de(p)).collect::<Result<Vec<_>>>()?;
    let mut e = omoikane::sync::Eleccion {
        preferencias: o.share_preferences,
        global: !o.no_global,
        proyectos: projects,
        espacios: Vec::new(),
        autor: if o.no_author { None } else { o.author.clone().or_else(omoikane::entorno::autor).or_else(omoikane::sync::nombre_git) },
    };
    if o.yes || !omoikane::entorno::puede_preguntar() {
        return Ok(e);
    }
    e.preferencias = ask_yes_no(&tr!("Share your preferences too? They are usually personal.", "¿Compartir también tus preferencias? Suelen ser personales."), e.preferencias)?;
    e.global = ask_yes_no(&tr!("Share your global memories?", "¿Compartir tus memorias globales?"), e.global)?;
    if e.proyectos.is_empty() {
        print!("{} ", tr!("Projects to share (Enter = all; or folders separated by commas):", "Proyectos que compartir (Intro = todos; o carpetas separadas por comas):"));
        std::io::stdout().flush()?;
        let mut l = String::new();
        std::io::stdin().read_line(&mut l)?;
        e.proyectos = l.split(',').map(str::trim).filter(|x| !x.is_empty()).map(|x| proyecto::de(std::path::Path::new(x))).collect::<Result<_>>()?;
    }
    if let Some(a) = e.autor.clone()
        && !ask_yes_no(&tr!("Write «{a}» as the author of each memory?", "¿Apuntar «{a}» como autor de cada memoria?"), true)?
    {
        e.autor = None;
    }
    Ok(e)
}

/// What happened when joining with a code from `omoi sync code`.
enum Union {
    /// Joined a new repository for this computer (its URL, without credentials).
    Joined(String, Option<String>),
    /// It already synced with it: the new key of a rotation was added.
    NewKey(u32),
    /// It already synced with it and already had that key.
    AlreadyHad,
    /// The person said no.
    Cancelled,
}

/// `omoi sync join <code>` (and each code of `OMOIKANE_SYNC_CODE`). With `confirm`, shows what will be
/// uploaded and, in a terminal, asks; without it (the variable is already the decision) it writes nothing to
/// the output, which may be the JSON of `omoi --json sync`. The code carries the key: it never appears in
/// any message.
#[allow(clippy::too_many_arguments)]
fn join_with(c: &Common, path: &std::path::Path, code: &str, other_url: Option<String>, another_person: bool, space: Option<String>, choice: &SyncOptions, confirm: bool) -> Result<Union> {
    use omoikane::sync;
    let mut db = open_db(c)?;
    let sync::Codigo { url, clave: key, id, persona: person, espacio: space_name } = sync::leer_codigo(code)?;
    let space_name = checked_space(space)?.or(space_name);
    if let Some(config) = sync::leer_config_de(&sync::rutas_de(path, space_name.as_deref()))? {
        // Joining as another person the general repository it already syncs with as the same one (a computer joined with
        // 1.8.5 or earlier): without turning it off first, the keys and the mark would stay as they were and the
        // other person's seals would keep counting as theirs.
        if another_person && space_name.is_none() && !config.another_person {
            bail!(tr!(
                "this computer already syncs this repository as the same person: run `omoi sync off` first, then join again with --another-person",
                "este equipo ya sincroniza este repositorio como la misma persona: haz antes `omoi sync off` y vuelve a unirte con --another-person"
            ));
        }
        // It already syncs with that repository. Another URL (a new token in `OMOIKANE_SYNC_URL`) becomes
        // its own before anything else: with the old one it may no longer get through.
        // If the code turns out to be for another repository, the URL goes back to the previous one.
        let old_url = match &other_url {
            Some(u) => sync::cambiar_url(path, space_name.as_deref(), u)?,
            None => None,
        };
        // The new key of a rotation, next to the old ones.
        let union = match sync::anadir_clave(&db, path, space_name.as_deref(), id, &key) {
            Ok(true) => Union::NewKey(id),
            Ok(false) => Union::AlreadyHad,
            Err(e) => {
                if let Some(v) = &old_url {
                    let _ = sync::cambiar_url(path, space_name.as_deref(), v);
                }
                return Err(e);
            }
        };
        if let Some(u) = other_url.as_deref().filter(|_| old_url.is_some()) {
            eprintln!("{}", tr!("the repository URL is now {}", "la URL del repositorio es ahora {}", sync::sin_credenciales(u)));
        }
        return Ok(union);
    }
    let url = other_url.unwrap_or(url);
    let e = choose(choice)?;
    // By default, another computer of the same person: their person, so that projects without
    // a git remote are merged even if each computer has a different author name (and the ones that were already
    // here move to it). With `--another-person`, this computer's: nothing is merged.
    let local = omoikane::db::persona_de(&db);
    let person_to_use = if another_person { Some(local.clone()) } else { person };
    let mut config = sync::config_con_persona(&url, &e, person_to_use.as_deref(), &local);
    config.global &= space_name.is_none();
    // Another person's general repository: its key is not only this one's (see `verificado::llaves`).
    config.another_person = another_person && space_name.is_none();
    config.space = space_name;
    if confirm && !confirm_upload(&db, &config, &e, choice.yes)? {
        return Ok(Union::Cancelled);
    }
    sync::unirse(&mut db, path, &config, id, &key)?;
    sync::adoptar_persona(&mut db, path, &config.person)?;
    Ok(Union::Joined(sync::sin_credenciales(&url), config.space))
}

/// `OMOIKANE_SYNC_CODE` (and `OMOIKANE_SYNC_URL`): joins, without asking, each repository of those
/// codes this computer doesn't sync with yet (or adds the new key of a rotation). It
/// reports it on the error output, without the code. Without the variable it does nothing.
fn join_from_env(c: &Common, path: &std::path::Path) -> Result<()> {
    let codes = omoikane::entorno::codigos_sync();
    let url = omoikane::entorno::url_sync();
    anyhow::ensure!(
        url.is_none() || codes.len() <= 1,
        tr!("OMOIKANE_SYNC_URL goes with a single code in OMOIKANE_SYNC_CODE", "OMOIKANE_SYNC_URL va con un solo código en OMOIKANE_SYNC_CODE")
    );
    // The defaults, without asking: the variable is already the decision.
    let options = SyncOptions { yes: true, share_preferences: false, no_global: false, project: Vec::new(), no_author: false, author: None };
    let several = codes.len() > 1;
    for (i, k) in codes.iter().enumerate() {
        let union = join_with(c, path, k, url.clone(), false, None, &options, false).map_err(|e| {
            // A code's error doesn't carry the code (nor its key): only which of them it was.
            if several { e.context(tr!("OMOIKANE_SYNC_CODE (code {})", "OMOIKANE_SYNC_CODE (código {})", i + 1)) } else { e.context("OMOIKANE_SYNC_CODE") }
        })?;
        match union {
            Union::Joined(u, _) => eprintln!("{}", tr!("joined {u} (OMOIKANE_SYNC_CODE)", "unido a {u} (OMOIKANE_SYNC_CODE)")),
            Union::NewKey(id) => eprintln!("{}", tr!("new key k{id} added (OMOIKANE_SYNC_CODE)", "añadida la clave nueva k{id} (OMOIKANE_SYNC_CODE)")),
            Union::AlreadyHad | Union::Cancelled => {}
        }
    }
    Ok(())
}

fn sync(c: &Common, action: Option<SyncAction>, background: bool, pull: bool) -> Result<()> {
    use omoikane::sync;
    let path = db_path(c);
    // These are Omoikane's internal ones: with an action, one of the two parts would be silently ignored.
    if (pull || background) && action.is_some() {
        bail!(tr!("`--pull` and `--background` don't go with an action: use them with `omoi sync` alone", "`--pull` y `--background` no van con una acción: úsalos solo con `omoi sync`"));
    }
    // A CI or a server with `OMOIKANE_SYNC_CODE`: it joins by itself before syncing.
    if pull || action.is_none() && !background {
        join_from_env(c, &path)?;
    }
    if pull {
        return sync::traer(&path, || open_db(c));
    }
    if background {
        // Without the model: whatever arrives without a vector is completed by the next command that opens it.
        return sync::en_segundo_plano(&path, || Ok((open_db(c)?, None)));
    }
    let first = |notice: &str| -> Result<()> {
        let (mut db, model) = open_with_model(c)?;
        let current = project_of(c).ok();
        let r = sync::ahora_mismo(&mut db, &path, current.as_deref(), model.as_ref())?;
        if c.json {
            return json(&r);
        }
        println!("{notice}");
        println!("{}", tr!("  {} memories shared; {} received", "  {} memorias compartidas; {} recibidas", r.exportadas, r.nuevas));
        Ok(())
    };
    match action {
        None => {
            let (mut db, model) = open_with_model(c)?;
            let current = project_of(c).ok();
            let r = sync::ahora_mismo(&mut db, &path, current.as_deref(), model.as_ref())?;
            if c.json {
                return json(&r);
            }
            println!(
                "{}",
                tr!(
                    "{}: {} new, {} updated, {} forgotten from {} file(s); {} memories shared",
                    "{}: {} nuevas, {} actualizadas, {} olvidadas de {} fichero(s); {} memorias compartidas",
                    r.equipo,
                    r.nuevas,
                    r.actualizadas,
                    r.olvidadas,
                    r.leidos,
                    r.exportadas
                )
            );
            if r.de_version_nueva > 0 {
                println!("{}", sync::aviso_lineas_nuevas(r.de_version_nueva));
            }
            for a in &r.avisos {
                println!("{a}");
            }
            Ok(())
        }
        Some(SyncAction::Init { url, create, space, eleccion: choice }) => {
            let mut db = open_db(c)?;
            let space_name = checked_space(space)?;
            if sync::leer_config_de(&sync::rutas_de(&path, space_name.as_deref()))?.is_some() {
                bail!(tr!("sync is already set up here (`omoi sync status`; `omoi sync off` to start over)", "la sincronización ya está configurada aquí (`omoi sync status`; `omoi sync off` para empezar de nuevo)"));
            }
            let url = match (url, create) {
                (Some(u), _) => u,
                (None, Some(name)) => create_repository(&name)?,
                (None, None) => unreachable!("clap exige url o --create"),
            };
            let e = choose(&choice)?;
            // The person: if this computer already syncs another repository, the one it already has; if this is the
            // first, the author's (or the git name), and if there is none, the local one.
            let local = omoikane::db::persona_de(&db);
            let already_syncing = !sync::repos(&path).is_empty();
            let mut config = sync::config_con_persona(&url, &e, already_syncing.then_some(local.as_str()), &local);
            // A client's repository doesn't carry the global ones (they belong to the person, not the client).
            config.global &= space_name.is_none();
            config.space = space_name;
            if !confirm_upload(&db, &config, &e, choice.yes)? {
                return Ok(());
            }
            sync::iniciar(&mut db, &path, &config)?;
            sync::adoptar_persona(&mut db, &path, &config.person)?;
            drop(db);
            first(&tr!(
                "Sync set up. To add another computer, run `omoi sync code` here and `omoi sync join <code>` there.",
                "Sincronización configurada. Para añadir otro equipo: `omoi sync code` aquí y `omoi sync join <código>` allí."
            ))
        }
        Some(SyncAction::Join { codigo: code, url: other_url, another_person, space, eleccion: choice }) => {
            let code = match code {
                Some(k) if k == "-" => {
                    let mut l = String::new();
                    std::io::stdin().read_line(&mut l)?;
                    l
                }
                Some(k) => k,
                // Without a code: those of `OMOIKANE_SYNC_CODE` (a CI, a server), without asking anything.
                None => {
                    anyhow::ensure!(
                        !omoikane::entorno::codigos_sync().is_empty(),
                        tr!("give the code from `omoi sync code` (or set OMOIKANE_SYNC_CODE)", "da el código de `omoi sync code` (o pon OMOIKANE_SYNC_CODE)")
                    );
                    anyhow::ensure!(
                        other_url.is_none() && space.is_none() && !another_person,
                        tr!("with OMOIKANE_SYNC_CODE, use OMOIKANE_SYNC_URL for another URL", "con OMOIKANE_SYNC_CODE, otra URL va en OMOIKANE_SYNC_URL")
                    );
                    join_from_env(c, &path)?;
                    return first(&tr!("Joined from OMOIKANE_SYNC_CODE.", "Unido desde OMOIKANE_SYNC_CODE."));
                }
            };
            match join_with(c, &path, &code, other_url, another_person, space, &choice, true)? {
                Union::AlreadyHad => {
                    println!("{}", tr!("this computer already has that key", "este equipo ya tiene esa clave"));
                    Ok(())
                }
                Union::Cancelled => Ok(()),
                Union::NewKey(id) => first(&tr!(
                    "New key k{id} added (the old ones stay, to read what others haven't re-encrypted yet).",
                    "Añadida la clave nueva k{id} (las viejas se quedan, para leer lo que otros aún no han vuelto a cifrar)."
                )),
                Union::Joined(url, space_name) => {
                    first(&tr!("Joined {}.", "Unido a {}.", url))?;
                    // What just arrived from the space, so that the agent has it in its next session.
                    if !c.json
                        && let Some(space_name) = &space_name
                        && let Some(t) = omoikane::llegada::al_unirse(&open_db(c)?, space_name)?
                    {
                        println!("{t}");
                    }
                    Ok(())
                }
            }
        }
        Some(SyncAction::Code { space }) => {
            println!("{}", sync::codigo(&path, checked_space(space)?.as_deref())?);
            eprintln!("{}", tr!("It carries the key to your memories: share it only through a private channel.", "Lleva la clave de tus memorias: pásalo solo por un canal privado."));
            Ok(())
        }
        Some(SyncAction::Status) => {
            let config = sync::leer_config(&path)?.map(|mut cf| {
                cf.url = sync::sin_credenciales(&cf.url);
                cf
            });
            let general = sync::rutas(&path);
            let spaces = sync::espacios(&path);
            let paused_list = if path.exists() { proyecto::en_pausa(&db::abrir(&path)?)? } else { Vec::new() };
            if c.json {
                let keys = |r: &sync::Rutas| {
                    let (ids, current_key) = sync::claves_de(r);
                    serde_json::json!({ "ids": ids, "current": current_key, "changed": sync::clave_cambiada(r) })
                };
                let of_spaces: Vec<serde_json::Value> = spaces
                    .iter()
                    .map(|e| {
                        let r = sync::rutas_de(&path, Some(e));
                        let cf = sync::leer_config_de(&r).ok().flatten().map(|mut cf| {
                            cf.url = sync::sin_credenciales(&cf.url);
                            cf
                        });
                        serde_json::json!({ "space": e, "config": cf, "state": sync::leer_estado_de(&r), "keys": keys(&r) })
                    })
                    .collect();
                return json(&serde_json::json!({
                    "config": config,
                    "state": sync::leer_estado(&path),
                    "pending": sync::pendiente(&path),
                    "paused": paused_list.iter().map(|x| {
                        let mut v = serde_json::to_value(x).unwrap_or_default();
                        v["text"] = sync::texto_pausa(&path, x).into();
                        v
                    }).collect::<Vec<_>>(),
                    "keys": config.as_ref().map(|_| keys(&general)),
                    "spaces": of_spaces,
                }));
            }
            println!("{}", sync_status(&path, None));
            for x in &paused_list {
                println!("{}{}", tr!("paused     ", "en pausa   "), sync::texto_pausa(&path, x));
            }
            for e in &spaces {
                let r = sync::rutas_de(&path, Some(e));
                println!("{}{}", tr!("space {e}: ", "espacio {e}: "), sync_status(&path, Some(e)));
                println!("{}{}", tr!("  keys     ", "  claves   "), sync_keys(&r));
            }
            if let Some(cf) = config {
                println!("{}{}", tr!("keys       ", "claves     "), sync_keys(&general));
                let kinds: Vec<&str> = cf.kinds.iter().map(|k| k.as_str()).collect();
                println!("{}{}", tr!("shares     ", "comparte   "), kinds.join(", "));
                println!("{}{}", tr!("global     ", "globales   "), if cf.global { tr!("yes", "sí") } else { "no".into() });
                match &cf.projects {
                    None => println!("{}", tr!("projects   all", "proyectos  todos")),
                    Some(p) => println!("{}{}", tr!("projects   ", "proyectos  "), p.join(", ")),
                }
                println!("{}{}", tr!("author     ", "autor      "), cf.author.as_deref().unwrap_or("-"));
                if let Some(err) = sync::leer_estado(&path).ultimo_error {
                    println!("{}{err}", tr!("last error ", "último error "));
                }
            }
            Ok(())
        }
        Some(SyncAction::Restore { id }) => restore_cli(c, id),
        Some(SyncAction::Rotate { space, yes }) => {
            let space_name = checked_space(space)?;
            if sync::leer_config_de(&sync::rutas_de(&path, space_name.as_deref()))?.is_none() {
                bail!(tr!("sync is not set up", "la sincronización no está configurada"));
            }
            if !yes {
                anyhow::ensure!(omoikane::entorno::puede_preguntar(), tr!("add --yes to change the key", "añade --yes para cambiar la clave"));
                if !ask_yes_no(
                    &tr!(
                        "Change the key? Computers that should keep reading will need the new code (`omoi sync join <code>`).",
                        "¿Cambiar la clave? Los equipos que deban seguir leyendo necesitarán el código nuevo (`omoi sync join <código>`)."
                    ),
                    false,
                )? {
                    return Ok(());
                }
            }
            let (mut db, model) = open_with_model(c)?;
            let current = project_of(c).ok();
            let (id, r) = sync::rotar(&mut db, &path, space_name.as_deref(), current.as_deref(), model.as_ref())?;
            let code = sync::codigo(&path, space_name.as_deref())?;
            if c.json {
                return json(&serde_json::json!({ "key": id, "exported": r.exportadas, "code": code }));
            }
            println!(
                "{}",
                tr!(
                    "New key k{id} in use: {} memories re-encrypted with it. On each computer that should keep reading, run `omoi sync join <code>` with this code:",
                    "Clave nueva k{id} en uso: {} memorias vueltas a cifrar con ella. En cada equipo que deba seguir leyendo, `omoi sync join <código>` con este código:",
                    r.exportadas
                )
            );
            println!("{code}");
            eprintln!("{}", tr!("It carries the key to your memories: share it only through a private channel.", "Lleva la clave de tus memorias: pásalo solo por un canal privado."));
            Ok(())
        }
        Some(SyncAction::Off { yes, space }) => {
            let db = open_db(c)?;
            let space_name = checked_space(space)?;
            if sync::leer_config_de(&sync::rutas_de(&path, space_name.as_deref()))?.is_none() {
                println!("{}", tr!("sync is not set up", "la sincronización no está configurada"));
                return Ok(());
            }
            if !yes {
                anyhow::ensure!(omoikane::entorno::puede_preguntar(), tr!("add --yes to stop syncing", "añade --yes para dejar de sincronizar"));
                if !ask_yes_no(&tr!("Stop syncing on this computer? Its memories stay; the key is deleted here.", "¿Dejar de sincronizar en este equipo? Sus memorias se quedan; la clave se borra de aquí."), false)? {
                    return Ok(());
                }
            }
            sync::desactivar(&db, &path, space_name.as_deref())?;
            println!("{}", tr!("sync is off on this computer", "sincronización apagada en este equipo"));
            Ok(())
        }
        Some(SyncAction::ForgetSealKeys { yes }) => {
            if let Some(var) = omoikane::verificado::shell_de_agente() {
                bail!(tr!(
                    "forgetting verification keys is for a person, and this looks like a coding agent's shell ({var} is set): run it in your own terminal",
                    "olvidar las claves de verificación es cosa de una persona, y esto parece la shell de un agente ({var} está puesta): hazlo en tu terminal"
                ));
            }
            let db = open_db(c)?;
            if !yes {
                anyhow::ensure!(omoikane::entorno::puede_preguntar(), tr!("add --yes to forget them", "añade --yes para olvidarlas"));
                if !ask_yes_no(
                    &tr!(
                        "Forget the verification keys of earlier general repositories? Your verifications signed only with them stop counting here until you verify them again.",
                        "¿Olvidar las claves de verificación de repositorios generales anteriores? Tus verificaciones firmadas solo con ellas dejan de contar aquí hasta que las vuelvas a verificar."
                    ),
                    false,
                )? {
                    return Ok(());
                }
            }
            let n = omoikane::verificado::olvidar_claves_apuntadas(&db)?;
            if c.json {
                println!("{}", serde_json::json!({ "forgotten": n }));
            } else {
                println!("{}", tr!("forgot {} verification key(s)", "olvidadas {} claves de verificación", n));
            }
            Ok(())
        }
    }
}

/// With `OMOIKANE_LANG=es`, the help in Spanish. The English texts are clap's doc comments.
fn in_spanish(c: Command) -> Command {
    if !idioma::es() {
        return c;
    }
    let c = c
        .about("Memoria local para agentes de IA: guarda lo que aprenden y se lo devuelve cuando hace falta.")
        .mut_arg("db", |a| a.help("Base de datos (por defecto OMOIKANE_DB, u omoikane.db en OMOIKANE_HOME, que por defecto es ~/.omoikane).").value_name("FICHERO"))
        .mut_arg("dir", |a| a.help("Carpeta cuyo proyecto se usa (por defecto, la actual).").value_name("CARPETA"))
        .mut_arg("json", |a| a.help("Salida en JSON, para otros programas."));
    // (subcommand, description, [(argument, help)])
    type Texts = [(&'static str, &'static str, &'static [(&'static str, &'static str)])];
    let texts: &Texts = &[
        ("remember", "Guarda una memoria. El contenido va como argumento o por la entrada estándar.", &[
            ("kind", "fact, decision, gotcha, preference o procedure."),
            ("title", "Una línea que diga de qué va."),
            ("content", "Qué, por qué y dónde. Sin él (o con «-»), se lee de la entrada estándar."),
            ("paths", "Fichero o carpeta del proyecto al que se refiere (se puede repetir)."),
            ("tags", "Etiqueta libre (se puede repetir). `review:AAAA-MM-DD` la marca para revisar ese día."),
            ("pinned", "Se inyecta siempre en su proyecto."),
            ("global", "Vale para todos los proyectos, no solo para este."),
            ("space", "Vale para todos los proyectos de un espacio: el de este proyecto (`--space`) o el que nombres (`--space=<nombre>`)."),
            ("trades", "Memoria de oficio (android, web, wordpress…; se puede repetir): la ven todos los proyectos de ese oficio, de cualquier cliente, y ningún otro. Solo conocimiento del stack, nada de un proyecto ni de un cliente."),
            ("para", "Con --trade, para quién es: client (solo proyectos en un espacio, de un cliente), personal (solo proyectos sin espacio) o both (por defecto)."),
            ("source", "Quién la guarda (por ejemplo «claude-code:<sesión>»)."),
            ("force", "Guardarla aunque ya exista una muy parecida (no se salta el aviso de fugas de --trade)."),
            ("allow_leaks", "Con --trade: guardarla aunque el aviso de fugas marque algo que has comprobado que es un falso positivo (nunca deja pasar una ruta absoluta ni la URL de un repositorio)."),
            ("key", "Una clave fija (p. ej. kumi-perfil): crea o actualiza la única memoria con esa clave en su nivel (este proyecto, o el que digan --space, --trade o --global), sin buscar duplicadas."),
            ("data", "Datos estructurados para programas, en JSON: van con la memoria, fuera de la búsqueda y del recall."),
        ]),
        ("search", "Busca en las memorias de este proyecto y en las globales.", &[
            ("query", "Lo que buscas, en lenguaje normal."),
            ("kind", "Solo de este tipo."),
            ("limit", "Cuántos resultados."),
            ("tags", "Solo las que llevan esta etiqueta (se puede repetir)."),
            ("all", "En todos los proyectos (de su espacio, si el proyecto está en uno)."),
            ("project", "En este proyecto: su carpeta, o su nombre o parte de él."),
            ("space", "En este espacio."),
        ]),
        ("get", "Una memoria entera (basta con el principio del id).", &[
            ("id", "Id de la memoria (basta con el principio)."),
            ("key", "En vez del id, la clave con la que se guardó (`remember --key`): la que ve este proyecto."),
            ("resolve", "Con --key: todas las de esa clave que ve este proyecto, de la más concreta a la más general (proyecto, espacio, oficio, global), cada una con su ámbito y sus datos, sin juntarlas."),
        ]),
        ("update", "Cambia una memoria. La versión anterior queda en el historial.", &[
            ("id", "Id de la memoria (basta con el principio)."),
            ("title", "Título nuevo."),
            ("content", "Contenido nuevo («-» para leerlo de la entrada estándar)."),
            ("kind", "Tipo nuevo."),
            ("pin", "Fijarla: se inyecta siempre en su proyecto."),
            ("unpin", "Desfijarla."),
            ("paths", "Sustituye las rutas por estas (se puede repetir)."),
            ("no_paths", "Quita todas las rutas."),
            ("tags", "Sustituye las etiquetas por estas (se puede repetir)."),
            ("no_tags", "Quita todas las etiquetas."),
            ("project", "Llevarla al proyecto de esta carpeta."),
            ("global", "Hacerla global: vale para todos los proyectos."),
            ("space", "Llevarla a este espacio: vale para todos sus proyectos."),
            ("trades", "Hacerla de oficio (se puede repetir): la ven todos los proyectos de ese oficio. Una personal conserva su id, su historial y sus veredictos; la de un cliente (en un espacio) pasa a una memoria de oficio NUEVA y la original se queda."),
            ("para", "En una memoria de oficio, para quién es: client (proyectos en un espacio), personal (proyectos sin espacio) o both."),
            ("no_trade", "Deja de ser de oficio (se queda como global corriente, no vuelve a su proyecto)."),
            ("allow_leaks", "En una memoria de oficio: guardar aunque el aviso de fugas marque algo que has comprobado que es un falso positivo (nunca deja pasar una ruta absoluta ni la URL de un repositorio)."),
            ("reason", "Por qué cambia (se guarda con la versión anterior)."),
            ("replace", "Trozo exacto del contenido que se cambia; tiene que aparecer exactamente una vez. Cada uno con su --with (se puede repetir)."),
            ("with", "Lo que va en lugar del --replace que le toca (se puede repetir, en el mismo orden)."),
            ("append", "Texto que se añade al final del contenido, tras una línea en blanco (un paso más en una receta)."),
            ("data", "Cambia sus datos estructurados por este JSON (fuera de la búsqueda y del recall)."),
            ("no_data", "Quita sus datos estructurados."),
        ]),
        ("forget", "Olvida una memoria: va a la papelera con su historial (`omoi restore` la devuelve; se vacía a los 30 días).", &[
            ("id", "Id de la memoria (basta con el principio)."),
            ("yes", "Sin preguntar. Sin él, omoi pregunta en una terminal y se niega en cualquier otro sitio."),
            ("purge", "Sin pasar por la papelera: la borra del todo."),
        ]),
        ("restore", "Devuelve una memoria de la papelera, en este equipo o en cualquiera sincronizado (sin id, enseña la papelera que se ve desde aquí).", &[("id", "Id de la memoria (sus 4 primeros caracteres o más).")]),
        ("trash", "Lo que hay en la papelera, visto desde aquí (desde el proyecto de un cliente, su espacio y las globales): memorias olvidadas en este equipo o en otro, 30 días (OMOIKANE_TRASH_DAYS).", &[
            ("empty", "Vaciarla ahora (lo que se ve desde aquí)."),
            ("yes", "Sin preguntar antes de vaciar."),
        ]),
        ("history", "Versiones anteriores de una memoria.", &[("id", "Id de la memoria (basta con el principio).")]),
        ("verify", "Marca una memoria como verificada por ti, una persona (el resto lo escriben agentes). El recall, la búsqueda y get lo enseñan; cambiar su texto lo quita. Un agente no puede hacerlo.", &[
            ("id", "Id de la memoria (basta con el principio)."),
            ("by", "Quién la verifica. Por defecto, tu nombre en la sincronización (OMOIKANE_AUTHOR o con el que te uniste), o el usuario del sistema."),
            ("unverify", "Quitarle la verificación."),
            ("yes", "Sin preguntar (para un programa que llevas tú, después de aprobarlo ahí). Sin él, omoi pregunta en una terminal y se niega en cualquier otro sitio. Dentro de la shell de un agente se niega siempre."),
        ]),
        ("review", "Lo que conviene revisar: memorias que otra más nueva contradice, estados viejos, rutas rotas, sin uso y demasiado largas.", &[
            ("all", "De todos los proyectos."),
            ("dismiss", "«Está bien así»: deja de avisar de esta memoria (hasta que cambie). Su id."),
            ("kind", "Con --dismiss: qué aviso (outdated, trade_duplicate, stale_status, code_drift, broken_path, unused, too_long, secret, due, review_date)."),
            ("other", "Con --dismiss en un aviso outdated: el id de la memoria más nueva."),
        ]),
        ("recent", "Las últimas memorias guardadas o cambiadas.", &[
            ("limit", "Cuántas."),
            ("kind", "Solo de este tipo."),
            ("all", "De todos los proyectos (de su espacio, si el proyecto está en uno)."),
            ("project", "De este proyecto: su carpeta, o su nombre o parte de él."),
            ("space", "De este espacio."),
        ]),
        ("diary", "El diario de sesiones: una línea por sesión (cuándo, en qué proyecto, ficheros tocados, memorias recibidas, aplicadas y guardadas), de la más reciente a la más vieja. Solo de este equipo.", &[
            ("limit", "Cuántas."),
            ("since", "Sesiones que acabaron ese día o después (2026-11-03, o 7d para los últimos 7 días)."),
            ("until", "Sesiones que empezaron ese día o antes."),
            ("file", "Solo las sesiones que tocaron un fichero cuya ruta contiene esto."),
            ("all", "De todos los proyectos (de su espacio, si el proyecto está en uno)."),
            ("project", "De este proyecto: su carpeta, o su nombre o parte de él."),
            ("space", "De este espacio."),
        ]),
        ("skill", "Convierte una memoria en skill de este repositorio (`.agents/skills/<nombre>/SKILL.md` y `.claude/skills/<nombre>/SKILL.md`): al hacer commit la tienen todos los agentes y el equipo. Sin id, lista las skills de Omoikane del repositorio y cuáles están desfasadas o editadas a mano.", &[
            ("id", "La memoria: su id (o el principio), o unas palabras para encontrarla («publicar versión»). Sin ella, lista las skills del repositorio y las memorias que podrían serlo, con la orden de cada una."),
            ("suggest", "Solo las memorias que podrían ser skills (recetas, y las que el agente aplicó en 2 sesiones o más), con su id."),
            ("refresh", "Pone al día las skills cuya memoria cambió después (no las editadas a mano)."),
            ("to", "Dónde escribirla: both (por defecto), agents (`.agents/skills`) o claude (`.claude/skills`)."),
            ("force", "Sustituye una skill editada a mano o que no es de Omoikane, y la escribe aunque parezca llevar un secreto."),
            ("dry_run", "Dice lo que escribiría sin tocar nada."),
        ]),
        ("tags", "Las etiquetas de aquí que son variantes unas de otras (`idea` e `ideas`, `test` y `tests`), con cuántas memorias lleva cada una. Con --merge, una sustituye a la otra en todas las memorias de aquí.", &[
            ("merge", "Cambia la etiqueta DE por A en todas las memorias de aquí que la lleven (cada cambio guarda su historial)."),
            ("all", "En todos los proyectos (de su espacio, si el proyecto está en uno)."),
            ("project", "En este proyecto: su carpeta, o su nombre o parte de él."),
            ("space", "En este espacio."),
        ]),
        ("decisions", "Las decisiones abiertas que esperan a la persona (tipo decision, etiqueta `open`): primero las bloqueantes (etiqueta `blocking`), luego las de después.", &[
            ("blocking", "Solo las bloqueantes (sin la respuesta no se puede seguir)."),
            ("limit", "Cuántas."),
            ("all", "De todos los proyectos (de su espacio, si el proyecto está en uno)."),
            ("project", "De este proyecto: su carpeta, o su nombre o parte de él."),
            ("space", "De este espacio."),
            ("tags", "Solo las que lleven también esta etiqueta (se puede repetir: todas)."),
            ("resolve", "En vez de listar, resuelve esta decisión: se añade su respuesta, se quitan las etiquetas `open` y `blocking` y se pone `resolved`."),
            ("answer", "Con --resolve: la respuesta de la persona."),
            ("reason", "Con --resolve: por qué (se guarda con la versión anterior)."),
            ("allow_leaks", "Con --resolve, en una decisión de oficio: guardar la respuesta aunque el aviso de fugas marque un falso positivo (nunca una ruta absoluta ni la URL de un repositorio)."),
        ]),
        ("list", "Todas las memorias con estas etiquetas en este proyecto (las de su espacio y las globales), de la más nueva a la más vieja, sin búsqueda ni ranking: para programas que guardan algo con una etiqueta y tienen que encontrarlo siempre.", &[
            ("tags", "Etiqueta que tienen que llevar (se puede repetir: todas). Al menos una."),
            ("kind", "Solo de este tipo."),
            ("limit", "Como mucho tantas (por defecto, todas)."),
            ("all", "De todos los proyectos (de su espacio, si el proyecto está en uno)."),
            ("project", "De este proyecto: su carpeta, o su nombre o parte de él."),
            ("space", "De este espacio."),
        ]),
        ("file-history", "Lo que sabe el diario de sesiones de unos ficheros del proyecto: en cuántas sesiones cambió cada uno, cuándo por última vez, qué ficheros cambian con él y qué gotchas lo nombran. Para código como una puerta de riesgo; solo de este equipo.", &[
            ("files", "Ficheros, relativos al repositorio o absolutos. Sin ninguno (o con -), uno por línea de la entrada estándar, como la salida de `git diff --name-only`."),
        ]),
        ("docs", "La documentación propia del proyecto en Markdown (una carpeta docs/, una wiki, una bóveda de Obsidian), en solo lectura: el recall trae el apartado que toca con cada mensaje. Sin acción, la lista.", &[]),
        ("projects", "Proyectos con memorias (sin acción, la lista): mover uno a su carpeta nueva, o juntar dos.", &[]),
        ("space", "Espacios: grupos de proyectos (un cliente, un producto con varios repositorios) cuyas memorias no llegan a otros espacios.", &[]),
        ("trades", "Oficios: el stack de un proyecto (android, web, wordpress…), detectado en sus ficheros. Las memorias de oficio llegan a todos los proyectos de su oficio. Sin acción, los de este proyecto.", &[]),
        ("export", "Una copia fiel: todas las memorias con su historial, las marcas de olvido, los veredictos y la papelera, una línea JSON cada una (`omoi import` la restaura exacta).", &[
            ("all", "De todos los proyectos (por defecto, este y las globales)."),
            ("encrypt", "Cifra cada línea con la clave del repositorio de sincronización de su espacio (AES-SIV; fuera de todo espacio, la del general), para guardar la copia en cualquier sitio."),
            ("obsidian", "En su lugar: una nota Markdown por memoria en <vault>/omoikane/, para la vista de grafo de Obsidian (un espejo: se rehace en cada exportación). Por defecto, los proyectos personales y las globales."),
            ("okf", "En su lugar: un bundle OKF v0.2 (Open Knowledge Format) en esta carpeta, un concepto por memoria (un espejo: se rehace en cada exportación). Por defecto, los proyectos personales y las globales."),
            ("space", "Con --obsidian o --okf: solo este espacio (un cliente), sin las globales. Cada espacio, en su propia carpeta."),
            ("watch", "Con --obsidian: mantener el vault al día, exportando otra vez cuando cambie la base."),
            ("interval", "Con --watch: segundos entre comprobaciones."),
        ]),
        ("backup", "Copia la base ahora en <su carpeta>/backups/omoikane-<fecha>.db (también se hace sola una vez al día; se conservan OMOIKANE_BACKUPS copias, 7 por defecto).", &[("list", "Solo enseñar las copias que hay.")]),
        ("tui", "Ver, buscar, fijar y olvidar memorias en la terminal, y recuperarlas o borrarlas de la papelera.", &[("all", "Empezar con todos los proyectos que se ven desde aquí: su espacio (desde la carpeta de usuario, todos). Por defecto, este y las globales.")]),
        ("import", "Importa un fichero de `omoi export` (exacto: mismos ids, fechas, historial y veredictos; gana el más nuevo, así que se puede repetir), una exportación de engram, un bundle OKF (--okf) o lo editado en un vault de Obsidian (--obsidian).", &[
            ("fichero", "El fichero exportado."),
            ("okf", "En su lugar: un bundle OKF v0.2 (una carpeta de conceptos en Markdown, como el knowledge/ de un repositorio). Cada concepto pone al día la misma memoria si se importa otra vez. Al proyecto de esta carpeta o, con --space, a ese espacio."),
            ("obsidian", "En su lugar: traer lo editado en un vault hecho con `omoi export --obsidian`. Una nota editada pone al día su memoria (con historial); una nota nueva en la carpeta de un proyecto, con `kind:`, entra como memoria nueva allí. Después se vuelve a exportar el vault."),
            ("prefer", "Con --obsidian: una nota editada en Obsidian cuya memoria también cambió aquí desde la exportación (un conflicto) se queda con lo de Obsidian (`obsidian`; lo de Omoikane queda en el historial) o con lo de Omoikane (`omoikane`; el cambio de la nota se pierde). Sin esto, no se toca ninguna de las dos."),
            ("space", "Con --okf o --obsidian: a este espacio (un cliente), sin proyecto. Con --obsidian, el espacio del que se exportó el vault."),
            ("global", "Con --okf: a las memorias globales (las ven todos los proyectos). Hace falta para importar desde la carpeta de usuario."),
            ("trust_verified", "Conservar también el «verificada por una persona» que puso otro: en un bundle que no se exportó de esta base, o en un fichero cuyas verificaciones no firmaste tú (la exportación de otro). Nunca desde la shell de un agente."),
            ("yes", "Con --okf --space: importar sin preguntar aunque traiga memorias fijadas, verificadas o personales al espacio del cliente (su repositorio es compartido)."),
            ("force", "Importar también lo que parece una memoria ya guardada."),
            ("dry_run", "Solo decir qué se importaría."),
        ]),
        ("model", "El modelo de embeddings que permite buscar por significado.", &[]),
        ("reindex", "Rehace los índices de búsqueda (palabras y vectores) a partir de las memorias.", &[]),
        ("mcp", "Sirve la memoria a un agente de IA por MCP (stdio). Lo arrancan los propios agentes.", &[]),
        ("serve", "Sirve las mismas herramientas por HTTP en 127.0.0.1 (para scripts y apps que no hablan MCP) y una vista web de todas las memorias en http://127.0.0.1:7438/.", &[
            ("port", "El puerto."),
            ("open", "Abrir la vista web en el navegador (si ya hay un servidor en marcha, solo la abre)."),
            ("memory", "Abrir (con --open) o escribir el enlace a esta memoria: su panel, con el botón Verificar (`http://127.0.0.1:7438/#/memoria/<id>`). Su id o el principio."),
        ]),
        ("setup", "Prepara un agente para usar Omoikane: claude, codex, gemini, cursor, opencode, qwen, kiro, copilot, windsurf, antigravity, kimi, pi, amp, kilo o grok.", &[
            ("agente", "El agente: claude, codex, gemini, cursor, opencode, qwen, kiro, copilot, windsurf, antigravity, kimi, pi, amp, kilo o grok."),
            ("yes", "No preguntar antes de cambiar nada."),
            ("dry_run", "Solo enseñar lo que se haría."),
            ("no_auto_update", "Claude Code: no encender la actualización automática (la diaria de Omoikane y la del plugin)."),
        ]),
        ("upgrade", "Actualizar Omoikane: mirar ahora, o encender o apagar la actualización automática diaria.", &[
            ("check", "Solo decir si hay una versión más nueva."),
            ("on", "Encender la actualización automática diaria."),
            ("off", "Apagarla."),
        ]),
        ("recall", "Para los hooks de Claude Code: lee el JSON del hook y escribe las memorias que hay que inyectar.", &[
            ("session_start", "Al empezar la sesión (o tras compactar): preferencias y memorias fijadas."),
            (
                "session_end",
                "Al cerrar o compactar una sesión: la apunta en el diario de sesiones (`omoi diary`) y aparta lo que quizá valga la pena guardar de la conversación (`transcript_path`) para el agente de la sesión siguiente. No escribe nada en la salida.",
            ),
            ("file_open", "Antes de que una herramienta abra un fichero (el hook `PreToolUse` de Read, Edit y Write): las memorias ligadas a ese fichero por sus rutas y su historial en el diario de sesiones, una vez por sesión."),
            ("format", "El formato del hook del agente: claude (también Codex y Qwen), gemini, cursor, copilot, antigravity o plain."),
        ]),
        ("doctor", "Comprueba SQLite, FTS5 y sqlite-vec, y dice dónde está la base.", &[]),
        ("sync", "Comparte las memorias entre tus equipos (o con un equipo de personas) por un repositorio git privado tuyo. Sin acción, sincroniza ahora.", &[]),
    ];
    let c = texts.iter().fold(c, |c, (command, about, args)| {
        c.mut_subcommand(*command, |s| args.iter().fold(s.about(*about), |s, (arg, help_text)| s.mut_arg(*arg, |a| a.help(*help_text))))
    });
    let c = spanish_headings(c)
        .disable_version_flag(true)
        .arg(Arg::new("version").short('V').long("version").action(ArgAction::Version).help("Muestra la versión."))
        .disable_help_subcommand(true);
    let c = c.mut_subcommand("model", |m| {
        m.subcommand_help_heading("Acciones")
            .mut_subcommand("status", |s| s.about("Qué modelo hay instalado y cuál se usa (por defecto)."))
            .mut_subcommand("install", |s| {
                s.about("Descarga el modelo (unos 150 MB, una vez) y lo compila.").mut_arg("from", |a| {
                    a.help("Tomarlo de una carpeta con el modelo int8 en vez de descargarlo.").value_name("CARPETA")
                })
                .mut_arg("semantic", |a| {
                    a.help("Instalar el modelo de significado (EmbeddingGemma, unos 450 MB) en vez del ligero. Con --from, una carpeta con él en formato compacto o como lo deja sentence-transformers.")
                })
            })
            .mut_subcommand("clean", |s| s.about("Borra los modelos instalados que no se usan."))
            .mut_subcommand("semantic", |s| {
                s.about("Enciende o apaga el modelo de significado (EmbeddingGemma). Apagado, Omoikane usa solo el modelo ligero, como antes de la 1.2: más rápido y con menos memoria, pero no entiende las preguntas hechas con otras palabras.")
                    .mut_arg("on", |a| a.help("Encenderlo."))
                    .mut_arg("off", |a| a.help("Apagarlo y cerrar su servicio."))
                    .mut_arg("auto", |a| a.help("Que vuelva a decidir el tamaño de la memoria (por defecto)."))
                    .mut_arg("ram", |a| a.help("La RAM que puede usar al indexar, en MB (0: volver a la de por defecto). Con más, indexa más rápido por lotes; OMOIKANE_SEMANTIC_RAM manda por encima."))
            })
    });
    let with_sync_options = |s: Command| {
        s.mut_arg("yes", |a| a.help("No preguntar: usar estas opciones y las de por defecto."))
            .mut_arg("share_preferences", |a| a.help("Compartir también las preferencias (por defecto se quedan en este equipo)."))
            .mut_arg("no_global", |a| a.help("No compartir las memorias globales."))
            .mut_arg("project", |a| a.help("Compartir solo este proyecto (se puede repetir; por defecto, todos).").value_name("CARPETA"))
            .mut_arg("no_author", |a| a.help("No apuntar autor en cada memoria."))
            .mut_arg("author", |a| a.help("El autor que se apunta en cada memoria (por defecto, el user.name de git)."))
    };
    let c = c.mut_subcommand("docs", |m| {
        m.subcommand_help_heading("Acciones")
            .mut_subcommand("list", |s| s.about("Las carpetas de documentación de este proyecto, con lo que tienen indexado (por defecto)."))
            .mut_subcommand("add", |s| {
                s.about("Añade una carpeta de ficheros Markdown y la indexa ya. No se copia nada a la memoria ni se escribe nunca en los ficheros.")
                    .mut_arg("carpeta", |a| a.help("La carpeta (docs/, una wiki, una bóveda de Obsidian)."))
            })
            .mut_subcommand("remove", |s| {
                s.about("Deja de usar una carpeta (y se borra su índice).").mut_arg("carpeta", |a| a.help("La carpeta, como sale en `omoi docs list`."))
            })
            .mut_subcommand("refresh", |s| s.about("Pone el índice al día ahora (también se hace al empezar cada sesión)."))
            .mut_subcommand("test", |s| {
                s.about("Qué apartado traería el recall con este texto, sin apuntarlo como dado.").mut_arg("texto", |a| a.help("El mensaje que quieres probar."))
            })
    });
    let project_help_es = |a: Arg| a.help("El proyecto: su carpeta, o su nombre o parte de él (por defecto, el de esta carpeta).").value_name("PROYECTO");
    let c = c.mut_subcommand("trades", |m| {
        m.subcommand_help_heading("Acciones")
            .mut_subcommand("list", |s| s.about("Los oficios de este proyecto y de dónde salen (por defecto); con --all, los de todos.").mut_arg("all", |a| a.help("Todos los proyectos, y cuántas memorias de oficio tiene cada oficio.")))
            .mut_subcommand("add", |s| s.about("El proyecto tiene también estos oficios, aunque no se detecten.").mut_arg("oficios", |a| a.value_name("OFICIO").help("Oficios: android, web, wordpress, seo-geo…")).mut_arg("project", project_help_es))
            .mut_subcommand("remove", |s| s.about("El proyecto no tiene estos oficios, aunque se detecten.").mut_arg("oficios", |a| a.value_name("OFICIO").help("Oficios: android, web, wordpress, seo-geo…")).mut_arg("project", project_help_es))
            .mut_subcommand("set", |s| s.about("Exactamente estos oficios (sin ninguno, ninguno), y deja de detectarlos.").mut_arg("oficios", |a| a.value_name("OFICIO").help("Oficios: android, web, wordpress, seo-geo…")).mut_arg("project", project_help_es))
            .mut_subcommand("auto", |s| s.about("Olvida lo puesto a mano y los vuelve a detectar en sus ficheros.").mut_arg("project", project_help_es))
    });
    let c = c.mut_subcommand("projects", |m| {
        m.subcommand_help_heading("Acciones")
            .mut_subcommand("list", |s| s.about("Los proyectos con memorias, primero los de actividad más reciente (por defecto)."))
            .mut_subcommand("move", |s| {
                s.about("El proyecto vive ahora en otra carpeta (se movió sin un remoto git que lo reconozca), o pasa a un espacio: sus memorias lo siguen.")
                    .mut_arg("proyecto", |a| a.help("El proyecto: su carpeta vieja, o su nombre o parte de él."))
                    .mut_arg("carpeta", |a| a.help("Su carpeta nueva."))
                    .mut_arg("space", |a| a.help("Meterlo en este espacio.").value_name("ESPACIO"))
                    .mut_arg("force", |a| a.help("Juntarlo aunque la carpeta sea un proyecto de otro espacio, pasarlo de un espacio a otro, o meter en un espacio un proyecto en pausa (sus memorias cambian de espacio)."))
            })
            .mut_subcommand("keep", |s| {
                s.about("Un proyecto en pausa se queda aquí donde está y vuelve a sincronizar: cuando otro equipo dice que está en otro espacio (para aceptarlo: `omoi projects move <proyecto> --space <espacio> --force`), o cuando esta carpeta se unió al proyecto de un espacio solo porque lo dice el repositorio de ese espacio (es de ese espacio; si es tuyo: `omoi space unassign <proyecto> --force`).")
                    .mut_arg("proyecto", |a| a.help("El proyecto: su carpeta, o su nombre o parte de él."))
            })
            .mut_subcommand("prune", |s| s.about("Olvida los proyectos sin memorias (una carpeta en la que vuelvas a trabajar vuelve sola)."))
            .mut_subcommand("distinct", |s| {
                s.about("Le dice a la revisión que A y B son proyectos distintos (o, sin B, que A es un proyecto de verdad aunque su carpeta parezca temporal): deja de proponer juntarlos. Solo en este equipo.")
                    .mut_arg("a", |a| a.help("Un proyecto (carpeta o nombre)."))
                    .mut_arg("b", |a| a.help("El otro proyecto (carpeta o nombre)."))
            })
            .mut_subcommand("merge", |s| {
                s.about("Junta el proyecto A en B: las memorias de A (con sus ids, historial y veredictos) y sus carpetas pasan a B.")
                    .mut_arg("a", |a| a.help("El proyecto que desaparece (carpeta o nombre)."))
                    .mut_arg("b", |a| a.help("El proyecto que se queda (carpeta o nombre)."))
                    .mut_arg("force", |a| a.help("Juntarlos aunque estén en espacios distintos (las memorias cambian de espacio)."))
            })
    });
    let c = c.mut_subcommand("space", |m| {
        m.subcommand_help_heading("Acciones")
            .mut_subcommand("list", |s| s.about("Los espacios, con sus proyectos (por defecto)."))
            .mut_subcommand("add", |s| s.about("Crea un espacio.").mut_arg("nombre", |a| a.help("Su nombre (un cliente, un producto…).")))
            .mut_subcommand("assign", |s| {
                s.about("Mete un proyecto en un espacio (por defecto, el de esta carpeta).")
                    .mut_arg("espacio", |a| a.help("El espacio."))
                    .mut_arg("proyecto", |a| a.help("El proyecto: su carpeta, o su nombre o parte de él."))
                    .mut_arg("force", |a| a.help("Moverlo aunque ya esté en otro espacio, o aunque esté en pausa (sus memorias van al repositorio de ese espacio)."))
            })
            .mut_subcommand("unassign", |s| {
                s.about("Saca un proyecto de su espacio (por defecto, el de esta carpeta).")
                    .mut_arg("proyecto", |a| a.help("El proyecto: su carpeta, o su nombre o parte de él."))
                    .mut_arg("force", |a| a.help("Hacerlo: sin espacio, sus memorias van al repositorio general (personal)."))
            })
    });
    let c = c.mut_subcommand("sync", |m| {
        m.subcommand_help_heading("Acciones")
            .mut_subcommand("init", |s| {
                with_sync_options(s.about("Configura un repositorio compartido nuevo (vacío o aún sin memorias): cualquier URL de git en la que puedas subir."))
                    .mut_arg("url", |a| a.help("El repositorio: git@github.com:tú/memoria.git, https://…, una carpeta…"))
                    .mut_arg("create", |a| {
                        a.help("Crearlo antes como repositorio privado con este nombre, con la línea de órdenes de GitHub (`gh`) o de GitLab (`glab`) si hay una instalada y con sesión iniciada.")
                            .value_name("NOMBRE")
                    })
                    .mut_arg("space", |a| a.help("Un repositorio (con su propia clave) solo para un espacio, además del general.").value_name("NOMBRE"))
            })
            .mut_subcommand("join", |s| {
                with_sync_options(s.about("Únete a un repositorio con el que ya sincroniza otro equipo, con el código de `omoi sync code`."))
                    .mut_arg("codigo", |a| a.help("El código que escribe `omoi sync code` («-» lo lee de la entrada estándar, para que no quede en el historial de la terminal; sin él, los de OMOIKANE_SYNC_CODE).").value_name("CÓDIGO"))
                    .mut_arg("url", |a| a.help("Llegar al mismo repositorio por otra URL (por ejemplo, SSH aquí y HTTPS en el otro equipo)."))
                    .mut_arg("another_person", |a| {
                        a.help("Este equipo es de otra persona (un compañero): sus proyectos sin remoto git no se juntan con los de quien invita. Por defecto, un equipo que se une es de la misma persona.")
                    })
                    .mut_arg("space", |a| a.help("El espacio de este repositorio (por defecto, el del código; ninguno: el general).").value_name("NOMBRE"))
            })
            .mut_subcommand("code", |s| {
                s.about("Escribe el código para unirse desde otro equipo. Lleva la clave vigente: pásalo por un canal privado.")
                    .mut_arg("space", |a| a.help("El código del repositorio de este espacio.").value_name("NOMBRE"))
            })
            .mut_subcommand("status", |s| s.about("Cómo está configurada la sincronización y cómo fue la última (todos los repositorios)."))
            .mut_subcommand("rotate", |s| {
                s.about("Cambia la clave del repositorio: vuelve a cifrar con una nueva las memorias de este equipo y escribe el código nuevo. Los equipos con solo la vieja dejan de leer lo que se escriba desde ahora.")
                    .mut_arg("space", |a| a.help("La clave del repositorio de este espacio.").value_name("NOMBRE"))
                    .mut_arg("yes", |a| a.help("No preguntar."))
            })
            .mut_subcommand("restore", |s| {
                s.about("Recupera una memoria que olvidó otro equipo (sin id, las enseña). Vuelve también su historial.")
                    .mut_arg("id", |a| a.help("Id de la memoria (basta con el principio)."))
            })
            .mut_subcommand("off", |s| {
                s.about("Deja de sincronizar en este equipo (sus memorias se quedan).")
                    .mut_arg("yes", |a| a.help("No preguntar."))
                    .mut_arg("space", |a| a.help("Solo el repositorio de este espacio.").value_name("NOMBRE"))
            })
            .mut_subcommand("forget-seal-keys", |s| {
                s.about("Olvida las claves de verificación apuntadas de repositorios generales anteriores. Para un equipo que se unió al general de un compañero con la 1.8.5 (sin --another-person apuntado): sus verificaciones seguían contando como tuyas. Las tuyas firmadas solo con una clave olvidada dejan de contar aquí hasta que las vuelvas a verificar.")
                    .mut_arg("yes", |a| a.help("No preguntar."))
            })
    });
    let names: Vec<String> = c.get_subcommands().map(|s| s.get_name().to_string()).collect();
    let c = names.iter().fold(c, |c, n| c.mut_subcommand(n, spanish_headings));
    ["model", "sync", "docs", "trades", "projects", "space"].iter().fold(c, |c, sub| {
        c.mut_subcommand(*sub, |m| {
            let actions: Vec<String> = m.get_subcommands().map(|s| s.get_name().to_string()).collect();
            actions.iter().fold(m.disable_help_subcommand(true), |m, n| m.mut_subcommand(n, spanish_headings))
        })
    })
}

/// clap's fixed help headings («Usage», «Commands», «Options»…) in Spanish.
fn spanish_headings(c: Command) -> Command {
    c.help_template("{about-with-newline}\nUso: {usage}\n\n{all-args}{after-help}")
        .subcommand_help_heading("Comandos")
        .mut_args(|a| {
            let heading = if a.is_positional() { "Argumentos" } else { "Opciones" };
            a.help_heading(heading)
        })
        .disable_help_flag(true)
        .arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::Help)
                .help("Muestra la ayuda.")
                .help_heading("Opciones"),
        )
}

/// `omoi setup codex|gemini|cursor|opencode`: changes the agent's configuration files.
fn setup_other(agent: &str, yes: bool, dry_run: bool) -> Result<()> {
    let changes = omoikane::agentes::plan(agent)?;
    if changes.is_empty() {
        println!("{}", tr!("{agent} already has Omoikane: nothing to do.", "{agent} ya tiene Omoikane: nada que hacer."));
        return Ok(());
    }
    println!("{}", tr!("To set up {agent}, omoi will change:", "Para preparar {agent}, omoi va a cambiar:"));
    for c in &changes {
        println!("  - {}   ({})", c.fichero.display(), c.que);
    }
    println!("{}", tr!("  (a copy of each file is kept as <file>.antes-de-omoikane)", "  (se guarda una copia de cada fichero como <fichero>.antes-de-omoikane)"));
    if dry_run {
        return Ok(());
    }
    if !yes && !ask()? {
        println!("{}", tr!("nothing changed", "no se ha cambiado nada"));
        return Ok(());
    }
    omoikane::agentes::aplicar(&changes)?;
    println!("{}", tr!("Done. Start a new {agent} session.", "Hecho. Abre una sesión nueva de {agent}."));
    Ok(())
}

/// «Go ahead?» in a terminal; outside one, --yes is needed.
fn ask() -> Result<bool> {
    if !omoikane::entorno::puede_preguntar() {
        bail!(tr!("add --yes to do it without asking", "añade --yes para hacerlo sin preguntar"));
    }
    eprint!("{}", tr!("Go ahead? [y/N] ", "¿Sigo? [s/N] "));
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes" | "s" | "si" | "sí"))
}

/// `omoi restore [id]` (and `omoi sync restore`, which is the same): without an id, the trash.
fn restore_cli(c: &Common, id: Option<String>) -> Result<()> {
    let Some(id) = id else {
        return list_trash(c, &open_db(c)?);
    };
    let (mut db, model) = open_with_model(c)?;
    // With the id given, like `forget` and `get`: whoever types it in their terminal already has it. What is
    // limited by folder is what is shown without asking (the list) and what is deleted in bulk.
    let before = omoikane::revisar::before(&db)?;
    let m = memoria::restore(&mut db, &id, model.as_ref())?;
    let _ = omoikane::revisar::refresh_for(&db, &before, std::slice::from_ref(&m.id));
    omoikane::sync::programar(&c.db.clone().unwrap_or_else(db::ruta_por_defecto));
    if c.json {
        return json(&m);
    }
    println!("{}", tr!("restored {}: {}", "recuperada {}: {}", short_id(&m.id), m.title));
    Ok(())
}

fn list_trash(c: &Common, db: &Connection) -> Result<()> {
    let (v, _) = omoikane::http::papelera_desde(db, &project_of(c)?)?;
    if c.json {
        return json(&v);
    }
    if v.is_empty() {
        println!("{}", tr!("the trash is empty", "la papelera está vacía"));
    }
    for o in v {
        println!("{}  {}  ({}, {})", short_id(&o.id), o.title.chars().take(90).collect::<String>(), o.by, o.deleted_at.get(..10).unwrap_or(&o.deleted_at));
    }
    Ok(())
}

/// In a terminal, it asks; anywhere else (an agent, a script) it refuses without `--yes`,
/// so that nobody deletes something for not having read the help.
fn confirm_forget(m: &Memory, purge: bool) -> Result<bool> {
    if !omoikane::entorno::puede_preguntar() {
        bail!(tr!("add --yes to forget {} without asking", "añade --yes para olvidar {} sin preguntar", short_id(&m.id)));
    }
    if purge {
        eprint!("{}", tr!("Delete «{}» and its history for good? It can't be undone [y/N] ", "¿Borrar «{}» y su historial del todo? No se puede deshacer [s/N] ", m.title));
    } else {
        eprint!("{}", tr!("Forget «{}»? It goes to the trash with its history; `omoi restore` brings it back [y/N] ", "¿Olvidar «{}»? Va a la papelera con su historial; `omoi restore` la devuelve [s/N] ", m.title));
    }
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes" | "s" | "si" | "sí"))
}
