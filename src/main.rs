//! `omoi`: memoria local para agentes de IA. Un binario, un fichero SQLite.

use anyhow::{Context, Result, bail};
use clap::{Arg, ArgAction, Args, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use omoikane::buscar::{self, Opciones};
use omoikane::memoria::{self, Ambito, Cambios, Memoria, Nueva, Tipo, Version};
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
    comun: Comun,
    #[command(subcommand)]
    orden: Orden,
}

#[derive(Args)]
struct Comun {
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

// `omoi diary`, con sus opciones aparte: dentro de `Orden`, la función que arma la ayuda de clap
// crecía tanto que desbordaba la pila (1 MB en Windows) en la compilación de depuración.
#[derive(Args)]
struct Diario {
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

// `omoi update`, con sus opciones aparte (como `omoi diary`): dentro de `Orden`, la función que arma
// la ayuda de clap desbordaba la pila en la compilación de depuración.
#[derive(Args)]
struct Cambio {
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
    kind: Option<Tipo>,
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
    datos: DatosCambio,
}

// `omoi decisions` y `omoi file-history`, aparte por lo mismo que `Diario`.
#[derive(Args)]
struct Decisiones {
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

// Los datos de `remember` y `update`, aparte por lo mismo que `Diario`.
#[derive(Args)]
struct DatosNuevos {
    /// Structured data for programs, as JSON: kept with the memory, out of search and recall.
    #[arg(long, value_name = "JSON")]
    data: Option<String>,
}

#[derive(Args)]
struct DatosCambio {
    /// Replace its structured data with this JSON (out of search and recall).
    #[arg(long, value_name = "JSON")]
    data: Option<String>,
    /// Remove its structured data.
    #[arg(long, conflicts_with = "data")]
    no_data: bool,
}

#[derive(Args)]
struct Lista {
    /// Tag the memories must have (repeatable: all of them). At least one.
    #[arg(long = "tag", value_name = "TAG", required = true)]
    tags: Vec<String>,
    /// Only this kind.
    #[arg(short, long)]
    kind: Option<Tipo>,
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

/// `omoi verify`: la marca de una persona (ver `verificado`). Aparte, para no agrandar `Orden`.
#[derive(Args)]
struct Verificar {
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
struct HistoriaFicheros {
    /// Files, relative to the repository or absolute. Without any (or with -), one per line from the standard input, such as the output of `git diff --name-only`.
    #[arg(value_name = "FILE")]
    files: Vec<String>,
}

#[derive(Subcommand)]
enum Orden {
    /// Save a memory. The content goes as an argument or through standard input.
    Remember {
        /// fact, decision, gotcha, preference or procedure.
        #[arg(short, long)]
        kind: Tipo,
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
        datos: DatosNuevos,
    },
    /// Search the memories of this project and the global ones.
    Search {
        /// What you are looking for, in plain words.
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// Only this kind.
        #[arg(short, long)]
        kind: Option<Tipo>,
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
    Update(Cambio),
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
    /// Bring back a memory from the trash, on this or any synced computer (without an id, list the trash).
    Restore {
        /// Memory id (its start is enough).
        id: Option<String>,
    },
    /// What is in the trash: memories forgotten here or elsewhere, kept 30 days (OMOIKANE_TRASH_DAYS).
    Trash {
        /// Empty it now.
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
    Verify(Verificar),
    /// The latest saved or changed memories.
    Recent {
        /// How many.
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// Only this kind.
        #[arg(short, long)]
        kind: Option<Tipo>,
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
    /// The session diary: one line per session (when, which project, files touched, memories recalled and saved), newest first. Only on this computer.
    Diary(Diario),
    /// Every memory with these tags in this project (its space's and the global ones), newest first, with no search or ranking: for programs that save something under a tag and must always find it again.
    List(Lista),
    /// Open decisions waiting for the user (kind decision, tag `open`): blocking ones (tag `blocking`) first, then the ones for later.
    Decisions(Decisiones),
    /// What the session diary knows about some files of this project: in how many sessions each changed, when last, which files change with it and which gotchas mention it. For code such as a risk gate; only on this computer.
    FileHistory(HistoriaFicheros),
    /// The project's own Markdown documentation (a docs/ folder, a wiki, an Obsidian vault), read-only: recall brings the section that fits a prompt. Without an action, the list.
    Docs {
        #[command(subcommand)]
        accion: Option<AccionDocs>,
    },
    /// Projects with memories (without an action, the list): move one to its new folder, or merge two.
    Projects {
        #[command(subcommand)]
        accion: Option<AccionProyectos>,
    },
    /// Spaces: groups of projects (a client, a product with several repositories) whose memories don't reach other spaces.
    #[command(alias = "spaces")]
    Space {
        #[command(subcommand)]
        accion: Option<AccionEspacio>,
    },
    /// Trades: a project's stack (android, web, wordpress…), detected from its files. Trade memories reach every project of their trade. Without an action, this project's.
    #[command(alias = "trade")]
    Trades {
        #[command(subcommand)]
        accion: Option<AccionOficios>,
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
        /// Encrypt every line with the sync key (AES-SIV), to keep the copy anywhere.
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
    /// Browse, search, pin and forget memories in the terminal.
    Tui {
        /// Start with every project (default: this one and the global ones).
        #[arg(long)]
        all: bool,
    },
    /// Import an `omoi export` file (exactly: same ids, dates, history and verdicts; newest wins, so it is safe to repeat), an engram export or an OKF bundle (--okf).
    Import {
        /// The export file.
        #[arg(required_unless_present = "okf")]
        fichero: Option<PathBuf>,
        /// Instead: an OKF v0.2 bundle (a folder of Markdown concepts, like a repository's knowledge/). Each concept updates the same memory when imported again. Into this folder's project, or with --space into that space.
        #[arg(long, value_name = "FOLDER", conflicts_with = "fichero")]
        okf: Option<PathBuf>,
        /// With --okf: into this space (a client), without a project.
        #[arg(long, requires = "okf")]
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
        accion: Option<AccionModelo>,
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
        accion: Option<AccionSync>,
        /// Run in the background after a change (Omoikane launches it itself).
        #[arg(long, hide = true)]
        background: bool,
        /// Bring in what other computers pushed, if anything (Omoikane launches it itself, at most every
        /// 2 minutes while you work).
        #[arg(long, hide = true)]
        pull: bool,
    },
    /// Set up an agent to use Omoikane: claude, codex, gemini, cursor, opencode, qwen, kiro, copilot, windsurf, antigravity, kimi, pi, amp or kilo.
    Setup {
        /// The agent: claude, codex, gemini, cursor, opencode, qwen, kiro, copilot, windsurf, antigravity, kimi, pi, amp or kilo.
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
        /// The agent's hook format: claude (also Codex and Qwen), gemini, cursor, copilot, antigravity or plain.
        #[arg(long, value_enum, default_value = "claude")]
        format: recall::Formato,
    },
    /// Check SQLite, FTS5 and sqlite-vec, and show where the database is.
    Doctor,
    /// For clients that don't speak MCP (the Claude Code plugin): runs one of the MCP tools with
    /// its JSON arguments on stdin and prints its result. Without a name, lists the tools.
    #[command(hide = true)]
    Tool {
        /// The tool: remember, search, get, update, forget, history, context or recent.
        nombre: Option<String>,
    },
}

#[derive(Subcommand)]
enum AccionProyectos {
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
enum AccionOficios {
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
enum AccionEspacio {
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
enum AccionSync {
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
        eleccion: OpcionesSync,
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
        eleccion: OpcionesSync,
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
struct OpcionesSync {
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
enum AccionDocs {
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
enum AccionModelo {
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
    // Rust ignora SIGPIPE y entonces `println!` revienta si se cierra la salida (`omoi … | head`).
    // Lo normal en Unix es terminar sin decir nada.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let mut cli = Cli::from_arg_matches(&en_espanol(Cli::command()).get_matches()).unwrap_or_else(|e| e.exit());
    // La base con ruta absoluta desde el principio: una relativa (`--db b/omoikane.db`) se lee desde
    // otras carpetas (git clona la sincronización desde la carpeta de la base) y no se encontraría.
    if let Some(d) = cli.comun.db.take() {
        cli.comun.db = Some(std::path::absolute(&d).unwrap_or(d));
    }
    // Sin carpeta de usuario (un contenedor) ni variables que digan dónde: un error claro, no una base
    // nueva en la carpeta en la que se ejecuta.
    // `setup` y `upgrade` no abren la base: no lo necesitan.
    let sin_base = matches!(cli.orden, Orden::Setup { .. } | Orden::Upgrade { .. });
    if let Some(e) = omoikane::entorno::casa_mala() {
        bail!(e);
    }
    // La base y los modelos, solo en este equipo: con `\\servidor\recurso` (o `//host/x`) en `--db`,
    // `OMOIKANE_DB` u `OMOIKANE_MODELS`, cada orden abriría ahí una conexión SMB.
    if !sin_base
        && (cli.comun.db.is_some() || omoikane::entorno::hay_donde_guardar())
        && let Some(e) = omoikane::entorno::ruta_mala(&tr!("the database", "la base"), &cli.comun.db.clone().unwrap_or_else(db::ruta_por_defecto))
            .or_else(|| omoikane::entorno::ruta_mala("OMOIKANE_MODELS", &omoikane::modelos::carpeta()))
    {
        bail!(e);
    }
    if !sin_base && cli.comun.db.is_none() && !omoikane::entorno::hay_donde_guardar() {
        bail!(omoikane::entorno::sin_casa());
    }
    let c = &cli.comun;
    if !matches!(cli.orden, Orden::Upgrade { .. }) {
        omoikane::actualizar::quizas_en_segundo_plano();
    }
    let escribe = matches!(cli.orden, Orden::Remember { .. } | Orden::Update(..) | Orden::Verify(..) | Orden::Forget { .. } | Orden::Restore { .. } | Orden::Import { .. } | Orden::Tui { .. });
    // Remember, update, forget y restore saben qué memoria tocan y ponen al día la revisión solo con
    // ella (`revisar::al_dia_de`); una importación o lo que se haga en la vista, entera.
    let sin_saber_que = matches!(cli.orden, Orden::Import { .. } | Orden::Tui { .. });
    let r = ejecutar(c, cli.orden);
    if escribe && r.is_ok() {
        // Lo que haya que revisar queda al día para el recall, que solo lee (ver `revisar`).
        if sin_saber_que && let Ok(db) = abrir(c) {
            let _ = omoikane::revisar::al_dia(&db);
        }
        omoikane::sync::programar(&ruta_db(c));
    }
    trazar_espera();
    r
}

/// Con `OMOIKANE_TRAZA`, cuánto esperó este proceso a que otro soltara la base (el test de
/// concurrencia lo suma por operación).
fn trazar_espera() {
    if std::env::var_os("OMOIKANE_TRAZA").is_some_and(|v| !v.is_empty()) {
        eprintln!("TRAZA espera_base {}", serde_json::json!({ "ms": db::esperado().as_secs_f64() * 1000.0 }));
    }
}

fn ruta_db(c: &Comun) -> PathBuf {
    c.db.clone().unwrap_or_else(db::ruta_por_defecto)
}

/// El contenido de `remember`: el argumento o la entrada estándar. Con `--data` y sin contenido, la
/// memoria es sobre todo sus datos: no se espera la entrada estándar y el contenido es el título.
fn contenido_de(content: Option<String>, title: &str, con_datos: bool) -> Result<String> {
    if con_datos && content.is_none() {
        return Ok(title.to_string());
    }
    texto_o_entrada(content)
}

/// `omoi export --okf`: aparte de `ejecutar` para no agrandar su marco de pila.
fn exportar_okf(c: &Comun, carpeta: &Path, space: Option<String>) -> Result<()> {
    let db = abrir(c)?;
    let que = match space {
        Some(s) => omoikane::obsidian::Que::Espacio(proyecto::buscar_espacio(&db, &s)?),
        None => omoikane::obsidian::Que::Personal,
    };
    let n = omoikane::okf::exportar(&db, carpeta, que)?;
    if c.json {
        return json(&n);
    }
    eprintln!(
        "{}",
        tr!(
            "{} memories in the OKF bundle {}: {} new, {} changed, {} unchanged, {} deleted",
            "{} memorias en el bundle OKF {}: {} nuevas, {} cambiadas, {} iguales, {} borradas",
            n.memories,
            carpeta.display(),
            n.created,
            n.updated,
            n.unchanged,
            n.deleted
        )
    );
    Ok(())
}

/// `omoi import --okf`: aparte de `ejecutar` para no agrandar su marco de pila.
fn importar_okf(c: &Comun, carpeta: &Path, space: Option<String>, global: bool, confiar: bool, si: bool, dry_run: bool) -> Result<()> {
    // Desde la carpeta personal, todo iría a lo global (lo ve cualquier proyecto): solo si se pide.
    let actual = proyecto_de(c)?;
    anyhow::ensure!(
        space.is_some() || global || !proyecto::es_general(&actual),
        tr!(
            "this is your home folder, not a project: importing here would make every concept global (seen from every project). Run it in the project's folder (or give --dir <project>), use --space <name> for a client, or --global if you do want them global",
            "esta es tu carpeta personal, no un proyecto: importar aquí haría globales todos los conceptos (los vería cualquier proyecto). Lánzalo en la carpeta del proyecto (o con --dir <proyecto>), usa --space <nombre> para un cliente, o --global si de verdad las quieres globales"
        )
    );
    let bundle = omoikane::okf::leer(carpeta)?;
    for a in &bundle.avisos {
        eprintln!("omoikane: {a}");
    }
    if let Some(v) = bundle.version.as_deref().filter(|v| !v.starts_with("0.")) {
        eprintln!("{}", tr!("omoikane: the bundle says OKF {v}; this version reads 0.2: importing what it understands", "omoikane: el bundle dice OKF {v}; esta versión lee la 0.2: se importa lo que entiende"));
    }
    let (mut db, modelo) = abrir_con_modelo(c)?;
    let destino = match space {
        Some(s) => omoikane::okf::Destino::Espacio(proyecto::buscar_espacio(&db, &s)?),
        None if global => omoikane::okf::Destino::Proyecto(String::new()),
        None => omoikane::okf::Destino::Proyecto(actual),
    };
    // Importar es cosa de la persona: el `verified` de una persona pone el sello, salvo dentro de la
    // shell de un agente (como `omoi verify`), y solo el de lo exportado de esta base salvo con
    // --trust-verified.
    let agente = omoikane::verificado::shell_de_agente();
    let op = omoikane::okf::Opciones { en_seco: dry_run, verificar: agente.is_none(), confiar_verificadas: confiar && agente.is_none() };
    // Al espacio de un cliente (su repositorio lo comparten otras personas): lo fijado, lo verificado y
    // lo personal (memorias de Omoikane de fuera de ese espacio) se avisa, y sin --yes se pregunta (sin
    // nadie delante, se para).
    if let omoikane::okf::Destino::Espacio(e) = &destino {
        let previa = omoikane::okf::importar_con(&mut db, modelo.as_ref(), &bundle, &destino, omoikane::okf::Opciones { en_seco: true, ..op })?;
        let clave = proyecto::clave_espacio(&db, Some(*e))?;
        let fuera = omoikane::okf::de_fuera_del_espacio(&bundle, &clave);
        if previa.pinned + previa.verified + fuera > 0 {
            let aviso = tr!(
                "this goes into the space «{clave}», whose sync repository other people share: {} pinned, {} verified by a person and {} memories from outside that space (personal or another client's) would come in",
                "esto va al espacio «{clave}», cuyo repositorio de sincronización comparten otras personas: entrarían {} fijadas, {} verificadas por una persona y {} memorias de fuera de ese espacio (personales o de otro cliente)",
                previa.pinned,
                previa.verified,
                fuera
            );
            eprintln!("omoikane: {aviso}");
            if !dry_run && !si {
                anyhow::ensure!(
                    omoikane::entorno::puede_preguntar(),
                    tr!("nothing was imported: add --yes if that is what you want", "no se ha importado nada: añade --yes si es lo que quieres")
                );
                if !preguntar_si(&tr!("Import them into the space?", "¿Importarlas al espacio?"), false)? {
                    bail!(tr!("nothing was imported", "no se ha importado nada"));
                }
            }
        }
    }
    let r = omoikane::okf::importar_con(&mut db, modelo.as_ref(), &bundle, &destino, op)?;
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
                match &destino {
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
    if let Some(var) = agente.filter(|_| r.not_verified > 0) {
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

/// `remember --key`: la de esa clave aquí, creada o actualizada, sin buscar duplicadas. Aparte de
/// `ejecutar` para no agrandar su marco de pila (en debug, cada rama suma).
fn recordar_por_clave(
    c: &Comun,
    db: &mut rusqlite::Connection,
    clave: &str,
    nueva: Nueva,
    datos: Option<serde_json::Value>,
    modelo: Option<&omoikane::embeber::Modelo>,
    antes: &omoikane::revisar::Antes,
) -> Result<()> {
    let r = memoria::guardar_por_clave(db, clave, nueva, datos, modelo)?;
    let _ = omoikane::revisar::al_dia_de(db, antes, std::slice::from_ref(&r.memoria.id));
    avisar_redactados(&r.memoria);
    avisar_datos_tapados(&r.tapados);
    if c.json {
        // La memoria con sus datos (`data`), si tiene, como `get --json`.
        let mut v = memoria::con_datos(db, vec![r.memoria])?.remove(0);
        v["created"] = serde_json::json!(r.creada);
        if !r.parecidas.is_empty() {
            v["similar_keys"] = serde_json::json!(r.parecidas);
        }
        return json(&v);
    }
    let id = corto(&r.memoria.id);
    println!("{}", if r.creada { tr!("saved {id}", "guardada {id}") } else { tr!("updated {id}", "actualizada {id}") });
    if !r.parecidas.is_empty() {
        println!("{}", tr!("  new key, but these keys already exist here: {}", "  clave nueva, pero aquí ya hay estas parecidas: {}", r.parecidas.join(", ")));
    }
    Ok(())
}

/// `get --key --resolve`: todas las capas de la clave para este proyecto, sin juntarlas.
fn capas_de_clave(c: &Comun, db: &rusqlite::Connection, clave: &str) -> Result<()> {
    let capas = memoria::capas_de_clave(db, &Ambito::de(db, &proyecto_de(c)?)?, clave)?;
    if c.json {
        return json(&capas);
    }
    if capas.is_empty() {
        println!("{}", tr!("no memory with key «{}» here", "aquí no hay ninguna memoria con la clave «{}»", clave.trim()));
    }
    for capa in &capas {
        println!("[{}] {}  {}", capa.scope, corto(&capa.id), capa.title);
        if let Some(d) = &capa.data {
            println!("  data: {d}");
        }
    }
    Ok(())
}

fn ejecutar(c: &Comun, orden: Orden) -> Result<()> {
    match orden {
        Orden::Doctor => doctor(c),
        Orden::Model { accion } => modelo(c, accion.unwrap_or(AccionModelo::Status)),
        Orden::Mcp => omoikane::mcp::servir(c.db.clone().unwrap_or_else(db::ruta_por_defecto), c.dir.clone()),
        Orden::Serve { port, open, memory } => {
            // El id entero, y que la vista de esta carpeta lo enseñe, antes de arrancar nada.
            let memory = match memory {
                Some(id) => {
                    let db = abrir(c)?;
                    Some(omoikane::http::memoria_del_enlace(&db, &proyecto_de(c)?, &id)?)
                }
                None => None,
            };
            omoikane::http::servir(c.db.clone().unwrap_or_else(db::ruta_por_defecto), c.dir.clone(), port, open, memory)
        }
        Orden::Tool { nombre } => {
            let mut args = String::new();
            if nombre.is_some() {
                std::io::stdin().read_to_string(&mut args)?;
            }
            let db = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
            // Quien llama dice quién es (el mod de Claude Code pone mcp:claude-code).
            let origen = std::env::var("OMOIKANE_SOURCE").ok().filter(|o| !o.trim().is_empty()).unwrap_or_else(|| "mcp".into());
            let (texto, es_error) = omoikane::mcp::herramienta(db, c.dir.clone(), nombre.as_deref(), &args, &origen)?;
            println!("{texto}");
            // Exit 1 si la herramienta respondió con isError: el cliente lo marca como error.
            trazar_espera();
            std::process::exit(if es_error { 1 } else { 0 })
        }
        Orden::Setup { agente, yes, dry_run, no_auto_update } => setup(&agente, yes, dry_run, no_auto_update),
        Orden::Upgrade { check, on, off, background, after } => upgrade(check, on, off, background, after),
        Orden::Sync { accion, background, pull } => sync(c, accion, background, pull),
        Orden::Recall { session_end: true, .. } => recall_al_cerrar(c),
        Orden::Recall { session_start, format, .. } => {
            // Un hook no debe molestar nunca: si algo falla, se dice por stderr y no se inyecta nada.
            if let Err(e) = recall(c, session_start, format) {
                eprintln!("omoikane: {e:#}");
            }
            Ok(())
        }
        Orden::Reindex => {
            let mut db = abrir(c)?;
            let n = memoria::reindexar_palabras(&mut db)?;
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
        Orden::Remember { kind, title, content, paths, tags, pinned, global, space, trades, para, source, force, allow_leaks, key, datos: DatosNuevos { data } } => {
            let content = contenido_de(content, &title, data.is_some())?;
            let data = data.as_deref().map(memoria::datos_de_texto).transpose()?;
            // Memoria de oficio: `--trade`, o una etiqueta `trade:` puesta a mano.
            let (oficios, tags) = oficios_pedidos(&trades, tags)?;
            let de_oficio = !oficios.is_empty();
            anyhow::ensure!(
                !(de_oficio && space.is_some()),
                tr!("a trade memory belongs to no project and no space: drop --space", "una memoria de oficio no es de ningún proyecto ni espacio: quita --space")
            );
            let global = global || de_oficio;
            let para = para.as_deref().map(omoikane::oficios::para).transpose()?.flatten();
            let tags = if de_oficio { omoikane::oficios::etiquetas_de_oficio(tags, &oficios, para.as_deref()) } else { tags };
            // Sin --dir, desde una carpeta que no es un proyecto (el home) la memoria va al repositorio
            // al que apunten sus rutas, o es global.
            let actual = proyecto_de(c)?;
            // Las preferencias son de la persona: globales, salvo que se diga el espacio o la carpeta (--dir).
            let global = global || (kind == Tipo::Preference && space.is_none() && c.dir.is_none());
            let (project, paths) =
                if global { (String::new(), paths) } else { proyecto::para_guardar(&actual, &paths, &format!("{title} {content}")) };
            // Desde una carpeta general se ve todo: ahí se buscan las duplicadas.
            let busca_en = if !project.is_empty() && proyecto::es_general(&actual) { actual.clone() } else { project.clone() };
            let (mut db, modelo) = abrir_con_modelo(c)?;
            // Todo lo que llevará: texto, rutas, etiquetas, clave y datos (`--force` no lo salta).
            if de_oficio {
                let campos = omoikane::oficios::campos_para_fugas(&[("title", &title), ("content", &content), ("paths", &paths.join(" "))], &tags, key.as_deref(), data.as_ref());
                avisar_fugas(c, &omoikane::oficios::bloquean(omoikane::oficios::fugas_en(&db, &campos)?, allow_leaks))?;
            }
            if !force
                && key.is_none()
                && let Some(modelo) = &modelo
                && let Some((existente, sim)) = if de_oficio {
                    // Entre las de su oficio, que desde lo global no se ven.
                    vectores::duplicada_de_oficio(&db, modelo, &oficios, &title, &content, None)?
                } else {
                    vectores::duplicada(&db, modelo, &busca_en, &title, &content)?
                }
            {
                if c.json {
                    #[derive(serde::Serialize)]
                    struct Parecida<'a> {
                        duplicate_of: &'a Memoria,
                        similarity: f32,
                    }
                    json(&Parecida { duplicate_of: &existente, similarity: sim })?;
                } else {
                    let id = corto(&existente.id);
                    println!(
                        "{}",
                        tr!(
                            "not saved: it looks like {id} «{}» (similarity {sim:.2})\n  change that one with `omoi update {id}`, or save anyway with --force",
                            "no guardada: se parece a {id} «{}» (similitud {sim:.2})\n  cambia esa con `omoi update {id}`, o guárdala igualmente con --force",
                            existente.title
                        )
                    );
                }
                // Código 2: no es un error, pero quien llame tiene que saber que no se guardó.
                std::process::exit(2);
            }
            let space_id = match space.as_deref().map(str::trim) {
                None => None,
                Some("") => Some(Ambito::de(&db, &actual)?.espacio().with_context(|| {
                    tr!("this project is not in a space: give its name (--space <name>), or `omoi space assign <space>`", "este proyecto no está en ningún espacio: di cuál (--space <nombre>), o `omoi space assign <espacio>`")
                })?),
                Some(n) => Some(proyecto::buscar_espacio(&db, n)?),
            };
            let antes = omoikane::revisar::antes(&db)?;
            // De dónde sale: la sesión y el commit (ver `procedencia`); las de oficio, sin nada del proyecto;
            // las globales y las de un espacio, solo la sesión (la rama de esta carpeta puede ser de un cliente).
            let provenance = if de_oficio {
                None
            } else if project.is_empty() || space_id.is_some() {
                omoikane::procedencia::solo_sesion(&db, &actual)
            } else {
                omoikane::procedencia::actual(&db, &project)
            };
            let nueva = Nueva { project, space_id, kind: Some(kind), title, content, paths, tags, pinned, source, provenance, ..Default::default() };
            if let Some(k) = key {
                return recordar_por_clave(c, &mut db, &k, nueva, data, modelo.as_ref(), &antes);
            }
            let m = memoria::guardar(&mut db, nueva, modelo.as_ref())?;
            let _ = omoikane::revisar::al_dia_de(&db, &antes, std::slice::from_ref(&m.id));
            avisar_redactados(&m);
            if let Some(d) = &data {
                avisar_datos_tapados(&memoria::poner_datos(&mut db, &m.id, Some(d.clone()))?);
            }
            let rel = vectores::relacionadas(&db, &busca_en, &m.title, &m.id, 3)?;
            if c.json {
                #[derive(serde::Serialize)]
                struct Guardada<'a> {
                    #[serde(flatten)]
                    memoria: &'a Memoria,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    data: Option<&'a serde_json::Value>,
                    #[serde(skip_serializing_if = "<[Memoria]>::is_empty")]
                    related: &'a [Memoria],
                }
                return json(&Guardada { memoria: &m, data: data.as_ref(), related: &rel });
            }
            println!("{}", tr!("saved {}", "guardada {}", corto(&m.id)));
            for r in &rel {
                println!("{}", tr!("  related: {} «{}»", "  relacionada: {} «{}»", corto(&r.id), r.title));
            }
            if m.content.chars().count() > recall::LARGO_PARA_RECALL {
                println!(
                    "{}",
                    tr!(
                        "  long memory: only its first {} characters count for recall; consider splitting it",
                        "  memoria larga: para el recall solo cuentan sus primeros {} caracteres; mejor partirla",
                        recall::LARGO_PARA_RECALL
                    )
                );
            }
            Ok(())
        }
        Orden::Search { query, kind, tags, limit, all, project, space } => {
            let (mut db, modelo) = abrir_con_modelo(c)?;
            let ambito = ambito_pedido(c, &db, all, project.as_deref(), space.as_deref())?;
            let all = !matches!(ambito, Ambito::Proyecto { .. });
            let o = Opciones { ambito, kind, tags, limite: limit };
            let res = buscar::buscar(&mut db, &query.join(" "), &o, modelo.as_ref())?;
            if c.json {
                return json(&res);
            }
            if res.is_empty() {
                println!("{}", tr!("nothing found", "nada"));
            }
            for r in &res {
                lista(&r.memoria, all);
            }
            Ok(())
        }
        Orden::Get { id, key, resolve } => {
            let db = abrir(c)?;
            if resolve && let Some(k) = &key {
                return capas_de_clave(c, &db, k);
            }
            let m = match (id, key) {
                (Some(id), _) => memoria::obtener(&db, &id)?,
                (None, Some(k)) => memoria::por_clave(&db, &Ambito::de(&db, &proyecto_de(c)?)?, &k)?
                    .with_context(|| tr!("no memory with key «{}» here", "aquí no hay ninguna memoria con la clave «{}»", k.trim()))?,
                (None, None) => unreachable!("clap pide id o --key"),
            };
            if c.json {
                return json(&memoria::con_datos(&db, vec![m])?.remove(0));
            }
            let datos = memoria::datos_de(&db, &[m.id.as_str()])?;
            entera(&m);
            if let Some(d) = datos.get(&m.id) {
                println!("\ndata: {}", serde_json::to_string_pretty(d)?);
            }
            Ok(())
        }
        Orden::Update(Cambio { id, title, content, kind, paths, no_paths, tags, no_tags, pin, unpin, project, global, space, trades, no_trade, para, allow_leaks, reason, replace, with, append, datos: DatosCambio { data, no_data } }) => {
            let data = if no_data { Some(None) } else { data.as_deref().map(memoria::datos_de_texto).transpose()?.map(Some) };
            anyhow::ensure!(
                replace.len() == with.len(),
                tr!("each --replace needs its --with ({} and {})", "cada --replace necesita su --with ({} y {})", replace.len(), with.len())
            );
            let cambios = Cambios {
                title,
                content: content.map(|t| texto_o_entrada(Some(t))).transpose()?,
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
            let (mut db, modelo) = abrir_con_modelo(c)?;
            let cambios = Cambios { space_id: space.as_deref().map(|s| proyecto::buscar_espacio(&db, s)).transpose()?, ..cambios };
            let (cambios, de_cliente) = oficios_al_cambiar(c, &db, &id, cambios, &trades, no_trade, para.as_deref(), data.as_ref(), allow_leaks)?;
            let antes = omoikane::revisar::antes(&db)?;
            // De un cliente a un oficio: una memoria nueva, y la del cliente se queda como estaba.
            if let Some(actual) = de_cliente {
                let tags = cambios.tags.clone().unwrap_or_else(|| actual.tags.clone());
                let m = omoikane::oficios::crear_de_oficio(&mut db, &actual, &cambios, tags, "cli", modelo.as_ref())?;
                if let Some(d) = data {
                    avisar_datos_tapados(&memoria::poner_datos(&mut db, &m.id, d)?);
                }
                let _ = omoikane::revisar::al_dia_de(&db, &antes, std::slice::from_ref(&m.id));
                avisar_redactados(&m);
                // Lo mismo promovido desde otro cliente: no se impide (ya está hecha), se dice.
                let duplicada = match &modelo {
                    Some(mo) => vectores::duplicada_de_oficio(&db, mo, &omoikane::oficios::de_etiquetas(&m.tags), &m.title, &m.content, Some(&m.id))?,
                    None => None,
                };
                if c.json {
                    let mut v = memoria::con_datos(&db, vec![memoria::obtener(&db, &m.id)?])?.remove(0);
                    v["promoted_from"] = serde_json::json!(actual.id);
                    if let Some((otra, sim)) = &duplicada {
                        v["duplicate_of"] = serde_json::to_value(otra)?;
                        v["similarity"] = serde_json::json!((sim * 1000.0).round() / 1000.0);
                    }
                    return json(&v);
                }
                if let Some((otra, sim)) = &duplicada {
                    println!(
                        "{}",
                        tr!(
                            "it looks like trade memory {} «{}» (similarity {sim:.2}): merge them (Merge in the Review tab of `omoi serve`, or `absorb` in the MCP `update`), or forget one",
                            "se parece a la memoria de oficio {} «{}» (similitud {sim:.2}): júntalas (Juntar en la pestaña Revisar de `omoi serve`, o `absorb` en el `update` del MCP), u olvida una",
                            corto(&otra.id),
                            otra.title
                        )
                    );
                }
                println!(
                    "{}",
                    tr!(
                        "created trade memory {} from {}: it came from a client, so the original stays in its project (with its history); forget it if it now only repeats this one",
                        "creada la memoria de oficio {} a partir de {}: venía de un cliente, así que la original se queda en su proyecto (con su historial); olvídala si ya solo repite esta",
                        corto(&m.id),
                        corto(&actual.id)
                    )
                );
                return Ok(());
            }
            // Solo los datos: sin versión nueva (como fijarla), y sin el «nada que cambiar» de `update`.
            let solo_datos = data.is_some() && sin_cambios(&cambios);
            let (m, cambiados) = if solo_datos {
                (memoria::obtener(&db, &id)?, Vec::new())
            } else {
                memoria::actualizar_con_cambios(&mut db, &id, cambios, modelo.as_ref())?
            };
            if let Some(d) = data {
                avisar_datos_tapados(&memoria::poner_datos(&mut db, &m.id, d)?);
            }
            let _ = omoikane::revisar::al_dia_de(&db, &antes, std::slice::from_ref(&m.id));
            avisar_redactados(&m);
            avisar_desverificada(&m);
            if c.json {
                let mut v = memoria::con_datos(&db, vec![memoria::obtener(&db, &m.id)?])?.remove(0);
                if !cambiados.is_empty() {
                    v["changed"] = serde_json::json!(cambiados);
                }
                if let Some(u) = &m.unverified {
                    v["unverified"] = serde_json::json!(u);
                }
                return json(&v);
            }
            println!("{}", tr!("updated {}", "actualizada {}", corto(&m.id)));
            for cambio in &cambiados {
                println!("  {}", tr!("changed:", "cambiado:"));
                for linea in cambio.context.lines() {
                    println!("    {linea}");
                }
            }
            Ok(())
        }
        Orden::Forget { id, yes, purge } => {
            let mut db = abrir(c)?;
            let m = memoria::obtener(&db, &id)?;
            if !yes && !confirmar_olvido(&m, purge)? {
                println!("{}", tr!("nothing deleted", "no se ha borrado nada"));
                return Ok(());
            }
            let antes = omoikane::revisar::antes(&db)?;
            let m = if purge { memoria::olvidar_del_todo(&mut db, &m.id)? } else { memoria::olvidar(&mut db, &m.id)? };
            let _ = omoikane::revisar::al_dia_de(&db, &antes, std::slice::from_ref(&m.id));
            if c.json {
                return json(&m);
            }
            if purge {
                println!("{}", tr!("deleted for good {}: {}", "borrada del todo {}: {}", corto(&m.id), m.title));
            } else {
                println!("{}", tr!("forgotten {}: {} (in the trash; `omoi restore {}` brings it back)", "olvidada {}: {} (en la papelera; `omoi restore {}` la devuelve)", corto(&m.id), m.title, corto(&m.id)));
            }
            Ok(())
        }
        Orden::Restore { id } => restaurar_cli(c, id),
        Orden::Verify(v) => verificar_cli(c, v),
        Orden::Trash { empty, yes } => {
            let db = abrir(c)?;
            if !empty {
                return listar_papelera(c, &db);
            }
            let n = memoria::papelera(&db)?.len();
            if !yes {
                anyhow::ensure!(omoikane::entorno::puede_preguntar(), tr!("add --yes to empty the trash without asking", "añade --yes para vaciar la papelera sin preguntar"));
                if !preguntar_si(&tr!("Empty the trash ({n} memories)? They can't be brought back afterwards.", "¿Vaciar la papelera ({n} memorias)? Después no se pueden recuperar."), false)? {
                    return Ok(());
                }
            }
            let n = memoria::vaciar_papelera(&db)?;
            if c.json {
                return json(&serde_json::json!({ "emptied": n }));
            }
            println!("{}", tr!("trash emptied: {n} memories deleted for good", "papelera vaciada: {n} memorias borradas del todo"));
            Ok(())
        }
        Orden::History { id } => {
            let db = abrir(c)?;
            let m = memoria::obtener(&db, &id)?;
            // Desde un proyecto, solo una memoria que se vea desde ahí (ver `memoria::historial_en`), con
            // sus oficios mirados (sin ellos no vería las memorias de su oficio).
            let aqui = proyecto_de(c)?;
            if let Err(e) = omoikane::oficios::preparar(&db, &aqui) {
                eprintln!("omoikane: trades: {e:#}");
            }
            let v = memoria::historial_en(&db, &Ambito::de(&db, &aqui)?, &m.id)?;
            if c.json {
                return json(&v);
            }
            historial(&m, &v);
            Ok(())
        }
        Orden::Recent { limit, kind, all, project, space } => {
            let db = abrir(c)?;
            let ambito = ambito_pedido(c, &db, all, project.as_deref(), space.as_deref())?;
            let all = !matches!(ambito, Ambito::Proyecto { .. });
            let v = memoria::recientes(&db, &ambito, kind, limit)?;
            if c.json {
                return json(&v);
            }
            if v.is_empty() {
                println!("{}", tr!("nothing yet", "nada todavía"));
            }
            for m in &v {
                lista(m, all);
            }
            Ok(())
        }
        Orden::Diary(d) => diario_cli(c, d),
        Orden::Decisions(d) => decisiones_cli(c, d),
        Orden::List(l) => {
            let db = abrir(c)?;
            let ambito = ambito_pedido(c, &db, l.all, l.project.as_deref(), l.space.as_deref())?;
            let todos = !matches!(ambito, Ambito::Proyecto { .. });
            let v = memoria::con_etiquetas(&db, &ambito, &l.tags, l.kind, l.limit.unwrap_or(usize::MAX))?;
            if c.json {
                return json(&memoria::con_datos(&db, v)?);
            }
            if v.is_empty() {
                println!("{}", tr!("nothing with those tags", "nada con esas etiquetas"));
            }
            for m in &v {
                lista(m, todos);
            }
            Ok(())
        }
        Orden::FileHistory(h) => historia_ficheros_cli(c, h),
        Orden::Docs { accion } => docs_cli(c, accion.unwrap_or(AccionDocs::List)),
        Orden::Review { all, dismiss, kind, other, refresh_drift, refresh_trade_duplicates } => {
            let db = abrir(c)?;
            if refresh_drift {
                // El proceso de fondo diario: la deriva y las duplicadas de oficio.
                omoikane::deriva::refrescar(&db)?;
                omoikane::revisar::refrescar_duplicadas_de_oficio(&db)?;
                return Ok(());
            }
            if refresh_trade_duplicates {
                omoikane::revisar::refrescar_duplicadas_de_oficio(&db)?;
                return Ok(());
            }
            if let Some(id) = dismiss {
                let otra = match other {
                    Some(o) => memoria::resolver_id(&db, &o)?,
                    None => String::new(),
                };
                let kind = kind.unwrap_or_else(|| if otra.is_empty() { omoikane::revisar::LARGA.into() } else { omoikane::revisar::DESFASADA.into() });
                omoikane::revisar::descartar(&db, &id, &kind, &otra)?;
                if kind == omoikane::revisar::DERIVA_CODIGO {
                    println!("{}", tr!("ok: it won't warn about it again until its files change a lot again", "hecho: no volverá a avisar hasta que sus ficheros vuelvan a cambiar mucho"));
                    return Ok(());
                }
                println!("{}", tr!("ok: it won't warn about it again until the memory changes", "hecho: no volverá a avisar hasta que la memoria cambie"));
                return Ok(());
            }
            let ambito = ambito_pedido(c, &db, all, None, None)?;
            let v = omoikane::revisar::avisos(&db, &ambito)?;
            if c.json {
                return json(&v);
            }
            // Los proyectos que ya no están en su carpeta, o cuyo `origin` cambió (de los que se ven desde aquí).
            let filtro = if matches!(ambito, Ambito::Todos) { None } else { Some(ambito.espacio()) };
            // Y los que están en otro espacio en otro equipo de la sincronización, con la orden, y los
            // que están en pausa, con las dos.
            let choques = omoikane::sync::choques(&ruta_db(c));
            let pausados = proyecto::en_pausa(&db)?;
            for p in proyecto::lista(&db, None, filtro, usize::MAX)? {
                if let Some(x) = pausados.iter().find(|x| x.key == p.key) {
                    println!("{:<12}  {}  {}", "project_paused", p.name, p.path);
                    println!("{:<12}  {}", "", omoikane::sync::texto_pausa(&ruta_db(c), x));
                }
                if p.gone {
                    println!("{:<12}  {}  {}", "project_gone", p.name, p.path);
                    println!("{:<12}  {}", "", tr!("the folder no longer exists: `omoi projects move {} <new folder>`", "la carpeta ya no existe: `omoi projects move {} <carpeta nueva>`", p.name));
                }
                if let Some(x) = choques.iter().find(|x| x.key == p.key) {
                    println!("{:<12}  {}  {}", "project_space", p.name, p.path);
                    println!("{:<12}  {}", "", x.texto());
                }
                if !p.origin_changed.is_empty() {
                    println!("{:<12}  {}  {}", "origin", p.name, p.path);
                    println!("{:<12}  {}", "", tr!("its git origin is now {} (its sync key stays {})", "su origin de git ahora es {} (su clave de sincronización sigue siendo {})", p.origin_changed, p.key));
                }
            }
            // Los proyectos duplicados o en carpetas temporales: se proponen, nunca se juntan solos.
            let propuestas = omoikane::nombres::propuestas(&db, &ambito)?;
            for x in &propuestas {
                let otro = if x.other.is_empty() { String::new() } else { format!("  →  {}  {}", x.other, x.other_path) };
                println!("{:<12}  {}  {}{otro}", format!("project_{}", x.kind), x.project, x.path);
                println!("{:<12}  {}", "", x.detail);
                let no = if x.other.is_empty() { tr!("a real project:", "es un proyecto de verdad:") } else { tr!("not the same:", "no son el mismo:") };
                println!("{:<12}  {}  ·  {no} {}", "", x.command, x.dismiss);
            }
            let cs = omoikane::candidatas::todas(&db)?;
            if v.is_empty() && cs.is_empty() && pausados.is_empty() && propuestas.is_empty() {
                println!("{}", tr!("nothing to review", "nada que revisar"));
            }
            if !cs.is_empty() {
                println!("{}", tr!("From your recent sessions, possibly worth keeping:", "De tus últimas sesiones, quizá valga la pena guardar:"));
                for x in &cs {
                    println!("  [{}] {}", x.kind, x.text.chars().take(160).collect::<String>());
                }
                println!();
            }
            for a in &v {
                let id: String = a.memory_id.chars().take(12).collect();
                println!("{:<12}  {id}  {}", a.kind, a.title.chars().take(90).collect::<String>());
                if let Some(t) = &a.other_title {
                    let etiqueta = match a.kind.as_str() {
                        omoikane::revisar::SUSTITUIDA if a.via > 1 => tr!("by (via {}):", "por (via {}):", a.via),
                        omoikane::revisar::SUSTITUIDA => tr!("by:", "por:"),
                        omoikane::revisar::CONTRADICE => tr!("against:", "frente a:"),
                        _ => tr!("newer:", "nueva:"),
                    };
                    println!("{:<12}  {etiqueta} {}  {}", "", a.other_id.chars().take(12).collect::<String>(), t.chars().take(80).collect::<String>());
                }
                println!("{:<12}  {}", "", a.detail);
            }
            Ok(())
        }
        Orden::Projects { accion } => proyectos_cli(c, accion.unwrap_or(AccionProyectos::List)),
        Orden::Space { accion } => espacios_cli(c, accion.unwrap_or(AccionEspacio::List)),
        Orden::Trades { accion } => oficios_cli(c, accion.unwrap_or(AccionOficios::List { all: false })),
        Orden::Tui { all } => {
            let (mut db, modelo) = abrir_con_modelo(c)?;
            let proyecto = proyecto_de(c)?;
            let all = all || proyecto::es_general(&proyecto);
            omoikane::tui::ejecutar(&mut db, modelo.as_ref(), proyecto, all)
        }
        Orden::Import { okf: Some(carpeta), space, global, trust_verified, yes, dry_run, .. } => importar_okf(c, &carpeta, space, global, trust_verified, yes, dry_run),
        Orden::Import { fichero, force, dry_run, trust_verified, .. } => {
            let fichero = fichero.context("missing file")?;
            // Un fichero de red no se abre (ver `proyecto::ruta_local`).
            proyecto::exigir_local(&fichero)?;
            let actual = proyecto_de(c)?;
            if omoikane::copia::es_propio(&fichero)? {
                let ruta = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
                let (mut db, modelo) = abrir_con_modelo(c)?;
                // Los sellos de verificada, como en `import --okf`: los que puso esta persona; los de otra,
                // solo con --trust-verified; y desde la shell de un agente, ninguno.
                let agente = omoikane::verificado::shell_de_agente();
                let sellos = omoikane::copia::Sellos { agente: agente.is_some(), confiar: trust_verified };
                let (cuenta, r) = omoikane::copia::importar(&mut db, &ruta, &fichero, &actual, modelo.as_ref(), dry_run, sellos)?;
                if agente.is_none() && cuenta.untrusted_verified > 0 {
                    eprintln!(
                        "{}",
                        tr!(
                            "omoikane: {} memories carry a person's verification that you didn't put (someone else's file, or an export from a computer without your general sync repository): they come in unverified. If you trust whoever made it, import again with --trust-verified",
                            "omoikane: {} memorias llevan la verificación de una persona que no pusiste tú (un fichero de otra persona, o exportado en un equipo sin tu repositorio general de sincronización): entran sin verificar. Si te fías de quien lo hizo, vuelve a importar con --trust-verified",
                            cuenta.untrusted_verified
                        )
                    );
                }
                if let Some(var) = agente.filter(|_| cuenta.unverified > 0) {
                    eprintln!(
                        "{}",
                        tr!(
                            "omoikane: {} memories carry a person's verification, but this looks like a coding agent's shell ({var} is set): they come in unverified. Import again from your own terminal to keep it",
                            "omoikane: {} memorias llevan la verificación de una persona, pero esto parece la shell de un agente ({var} está puesta): entran sin verificar. Vuelve a importar desde tu terminal para conservarla",
                            cuenta.unverified
                        )
                    );
                }
                if c.json {
                    return json(&serde_json::json!({ "file": cuenta, "new": r.nuevas, "updated": r.actualizadas, "forgotten": r.olvidadas, "skipped_lines": r.ilegibles, "dry_run": dry_run }));
                }
                if dry_run {
                    println!("{}", tr!("{} memories, {} forget marks, {} verdicts and {} in the trash to import (the file checks out)", "{} memorias, {} marcas de olvido, {} veredictos y {} en la papelera para importar (el fichero está bien)", cuenta.memories, cuenta.tombstones, cuenta.verdicts, cuenta.forgotten));
                } else {
                    println!("{}", tr!("imported: {} new, {} updated, {} forgotten; {} lines skipped; the rest was already here", "importado: {} nuevas, {} actualizadas, {} olvidadas; {} líneas saltadas; lo demás ya estaba", r.nuevas, r.actualizadas, r.olvidadas, r.ilegibles));
                }
                if cuenta.unknown > 0 {
                    println!("{}", omoikane::sync::aviso_lineas_nuevas(cuenta.unknown));
                }
                if !dry_run && r.nuevas + r.actualizadas > 0 {
                    omoikane::sync::programar(&ruta);
                }
                return Ok(());
            }
            let (entrantes, borradas) = omoikane::importar::leer(&fichero, c.dir.as_deref(), &actual)?;
            if dry_run {
                let mut por_proyecto: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
                for e in &entrantes {
                    *por_proyecto.entry(if e.nueva.project.is_empty() { "(global)".into() } else { e.nueva.project.clone() }).or_default() += 1;
                }
                println!("{}", tr!("{} memories to import ({borradas} deleted at the source, skipped):", "{} memorias para importar ({borradas} borradas en el origen, saltadas):", entrantes.len()));
                for (p, n) in por_proyecto {
                    println!("  {n:>5}  {p}");
                }
                return Ok(());
            }
            let (mut db, modelo) = abrir_con_modelo(c)?;
            let r = omoikane::importar::guardar(&mut db, modelo.as_ref(), entrantes, force)?;
            println!(
                "{}",
                tr!(
                    "imported {}; {} skipped because they were already saved (--force to import them anyway); {} deleted at the source; {} failed",
                    "importadas {}; {} saltadas porque ya estaban (--force para importarlas igualmente); {} borradas en el origen; {} fallidas",
                    r.guardadas,
                    r.duplicadas,
                    borradas,
                    r.fallidas
                )
            );
            Ok(())
        }
        Orden::Export { okf: Some(carpeta), space, .. } => exportar_okf(c, &carpeta, space),
        Orden::Export { obsidian: Some(vault), space, watch, interval, .. } => {
            let db = abrir(c)?;
            let que = match space {
                Some(s) => omoikane::obsidian::Que::Espacio(proyecto::buscar_espacio(&db, &s)?),
                None => omoikane::obsidian::Que::Personal,
            };
            let json_salida = c.json;
            let avisar = |n: &omoikane::obsidian::Cuenta| {
                if json_salida {
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
                }
            };
            if watch {
                return omoikane::obsidian::vigilar(&db, &vault, que, std::time::Duration::from_secs(interval), avisar);
            }
            avisar(&omoikane::obsidian::exportar(&db, &vault, que)?);
            Ok(())
        }
        Orden::Export { all, encrypt, .. } => {
            let mut salida = std::io::BufWriter::new(std::io::stdout().lock());
            let ruta = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
            let db = abrir(c)?;
            // Sin --all, lo que se ve desde el proyecto (también desde la carpeta del usuario: no todo).
            let ambito = if all {
                Ambito::Todos
            } else {
                match proyecto::resolver(&db, &proyecto_de(c)?)? {
                    Some((id, space)) => Ambito::Proyecto { id, space },
                    None => Ambito::Proyecto { id: -1, space: None },
                }
            };
            let n = omoikane::copia::exportar(&db, &ruta, &ambito, encrypt, &mut salida)?;
            salida.flush()?;
            eprintln!("{}", tr!("{} memories exported (with {} forget marks, {} verdicts and {} in the trash)", "{} memorias exportadas (con {} marcas de olvido, {} veredictos y {} en la papelera)", n.memories, n.tombstones, n.verdicts, n.forgotten));
            Ok(())
        }
        Orden::Backup { list } => {
            let ruta = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
            if !list {
                let destino = omoikane::copia::copiar(&abrir(c)?, &ruta)?;
                if !c.json {
                    println!("{}", tr!("copied to {}", "copiada en {}", destino.display()));
                }
            }
            let v = omoikane::copia::copias(&ruta);
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
fn upgrade(solo_mirar: bool, on: bool, off: bool, segundo_plano: bool, despues: Option<String>) -> Result<()> {
    use omoikane::actualizar::{self, Resultado};
    if let Some(anterior) = despues {
        actualizar::despues(&anterior)?;
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
    let r = actualizar::comprobar(&exe, !solo_mirar);
    if segundo_plano {
        return Ok(()); // nadie lo lee: lo que pase se ve en la sesión siguiente
    }
    match r? {
        Resultado::AlDia(v) => println!("{}", tr!("omoi {v} is the latest version.", "omoi {v} es la última versión.")),
        Resultado::NoSeInstala(v, por_que) if por_que.is_empty() => println!("{}", tr!("omoi {v} is available: `omoi upgrade` installs it.", "Hay omoi {v}: `omoi upgrade` la instala.")),
        Resultado::NoSeInstala(v, por_que) => println!("{}", tr!("omoi {v} is available, but not installed automatically: {por_que}", "Hay omoi {v}, pero no se instala sola: {por_que}")),
        Resultado::Actualizado(v) => println!("{}", tr!("Updated to omoi {v}.", "Actualizado a omoi {v}.")),
    }
    Ok(())
}

/// Lo que se pregunta al preparar Claude Code: si Omoikane se actualiza solo (el binario una vez al día
/// y el plugin con la actualización automática del marketplace). Sin terminal o con `--yes`, sí.
fn preguntar_auto_update(si: bool, no: bool, simulacro: bool) -> Result<()> {
    if simulacro {
        return Ok(());
    }
    let on = if no {
        false
    } else if let Some(ya) = omoikane::actualizar::preferencia().filter(|_| si || !omoikane::entorno::puede_preguntar()) {
        ya
    } else if si || !omoikane::entorno::puede_preguntar() {
        // Sin preguntar no se enciende: tiene que ser un sí de la persona.
        false
    } else {
        preguntar_si(
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

fn setup(agente: &str, si: bool, simulacro: bool, sin_auto_update: bool) -> Result<()> {
    if agente != "claude" {
        return setup_otro(agente, si, simulacro);
    }
    let pasos = omoikane::setup::plan()?;
    if pasos.is_empty() {
        println!("{}", tr!("Claude Code already has Omoikane: nothing to do.", "Claude Code ya tiene Omoikane: nada que hacer."));
        if omoikane::actualizar::preferencia().is_none() || sin_auto_update {
            preguntar_auto_update(si, sin_auto_update, simulacro)?;
        }
        return Ok(());
    }
    println!("{}", tr!("To set up Claude Code, omoi will:", "Para preparar Claude Code, omoi va a:"));
    for p in &pasos {
        println!("  - {}   (claude {})", p.que, p.args.join(" "));
    }
    if simulacro {
        return Ok(());
    }
    if !si && !preguntar()? {
        println!("{}", tr!("nothing changed", "no se ha cambiado nada"));
        return Ok(());
    }
    omoikane::setup::ejecutar(&pasos)?;
    preguntar_auto_update(si, sin_auto_update, simulacro)?;
    println!(
        "{}",
        tr!(
            "Done. Open a new Claude Code session: the memory tools and the automatic recall are there.",
            "Hecho. Abre una sesión nueva de Claude Code: ahí están las herramientas de memoria y el recall automático."
        )
    );
    Ok(())
}

/// `omoi recall --session-end`: apunta la sesión en el diario y aparta candidatas de la conversación.
/// Un fallo nunca molesta.
fn recall_al_cerrar(c: &Comun) -> Result<()> {
    let mut entrada = String::new();
    std::io::stdin().read_to_string(&mut entrada)?;
    let mut e: recall::Entrada = serde_json::from_str(&entrada).unwrap_or_default();
    let ruta_db = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
    // Cursor da la carpeta en `workspace_roots`; Copilot y Kiro no dan la ruta de la conversación.
    if e.cwd.is_empty()
        && let Some(raiz) = e.workspace_roots.first()
    {
        e.cwd = raiz.clone();
    }
    if e.transcript_path.is_empty()
        && let Some(p) = omoikane::candidatas::buscar_conversacion(&e.session_id)
    {
        e.transcript_path = p.to_string_lossy().into_owned();
    }
    let conversacion = Some(std::path::Path::new(&e.transcript_path)).filter(|p| !e.transcript_path.is_empty() && proyecto::existe(p));
    if (conversacion.is_none() && e.session_id.trim().is_empty()) || !ruta_db.exists() {
        return Ok(());
    }
    let project = match (&c.dir, e.cwd.is_empty()) {
        (Some(d), _) => proyecto::de(d)?,
        // El `cwd` del hook es la carpeta de la sesión del propio agente: puede ser de red (ver
        // `proyecto::de_confianza`). `--dir` llega de fuera y va por `proyecto::de`.
        (None, false) => proyecto::de_confianza(std::path::Path::new(&e.cwd))?,
        (None, true) => proyecto::actual()?,
    };
    let db = db::abrir(&ruta_db)?;
    // El diario primero: es lo más rápido, y si el hook se corta, queda apuntado. Ni un error ni un
    // pánico suyo cortan el hook ni dejan sin apartar las candidatas.
    let diario = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| omoikane::diario::apuntar(&db, &project, &e.session_id, conversacion)));
    match diario {
        Ok(Err(err)) => eprintln!("omoikane: {err:#}"),
        Err(_) => eprintln!("omoikane: the session diary failed"),
        Ok(Ok(_)) => {}
    }
    // La conversación se lee una vez para las candidatas y para los pasos de las recetas.
    let turnos = conversacion.and_then(|r| omoikane::candidatas::turnos(r).map_err(|err| eprintln!("omoikane: {err:#}")).ok());
    let pasos = turnos.as_deref().map(omoikane::recetas::pasos_de_turnos).unwrap_or_default();
    if let (Some(ruta), Some(t)) = (conversacion, turnos)
        && let Err(err) = omoikane::candidatas::apartar_con(&db, &project, &e.session_id, ruta, t)
    {
        eprintln!("omoikane: {err:#}");
    }
    // Lo último: los pasos de la sesión y, si alguna secuencia ya se repite lo bastante, una receta
    // propuesta para la siguiente. Tampoco corta nunca el hook.
    if conversacion.is_some() {
        let recetas = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            omoikane::recetas::apuntar(&db, &e.session_id, &pasos)?;
            omoikane::recetas::proponer(&db, &project, &e.session_id, &pasos)?;
            Ok(())
        }));
        match recetas {
            Ok(Err(err)) => eprintln!("omoikane: {err:#}"),
            Err(_) => eprintln!("omoikane: proposing recipes failed"),
            Ok(Ok(())) => {}
        }
    }
    Ok(())
}

fn recall(c: &Comun, al_empezar: bool, formato: recall::Formato) -> Result<()> {
    let mut entrada = String::new();
    std::io::stdin().read_to_string(&mut entrada)?;
    let mut e: recall::Entrada = serde_json::from_str(&entrada).unwrap_or_default();
    if e.cwd.is_empty()
        && let Some(raiz) = e.workspace_roots.first()
    {
        e.cwd = raiz.clone();
    }
    if e.prompt.is_empty()
        && let Ok(p) = std::env::var("USER_PROMPT")
    {
        e.prompt = p;
    }
    let project = match (&c.dir, e.cwd.is_empty()) {
        (Some(d), _) => proyecto::de(d)?,
        // El `cwd` del hook es la carpeta de la sesión del propio agente: puede ser de red (ver
        // `proyecto::de_confianza`). `--dir` llega de fuera y va por `proyecto::de`.
        (None, false) => proyecto::de_confianza(std::path::Path::new(&e.cwd))?,
        (None, true) => proyecto::actual()?,
    };
    let ruta = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
    if !ruta.exists() && !al_empezar {
        return Ok(()); // sin memorias todavía: nada que inyectar
    }
    let mut db = db::abrir(&ruta)?;
    let (evento, texto) = if al_empezar {
        // Una vez al día: la copia de la base y la purga de la papelera.
        omoikane::copia::diaria(&db, &ruta);
        let general = matches!(omoikane::sync::leer_config(&ruta), Ok(Some(_)));
        if general {
            let _ = omoikane::sync::adoptar(&db, &project);
        }
        // Con el repositorio general o con el de algún espacio (aunque solo haya espacios).
        if general || !omoikane::sync::espacios(&ruta).is_empty() {
            omoikane::sync::programar(&ruta);
            // Esa sincronización ya trae lo nuevo: el primer mensaje no lanza otra.
            omoikane::sync::apuntar_traido(&db);
        }
        let texto = recall::al_empezar(&mut db, &project, &e)?.map(|t| match omoikane::sync::aviso(&ruta) {
            // Antes del cierre del bloque, que es el último `</omoikane>`.
            Some(a) => match t.rfind("</omoikane>") {
                Some(i) => format!("{}{a}\n{}", &t[..i], &t[i..]),
                None => t,
            },
            None => t,
        });
        ("SessionStart", texto)
    } else {
        // El modelo abre en milisegundos (mapeado), y solo si el mensaje llega al canal por
        // significado; sin él, el recall va solo por palabras.
        let modelo = std::cell::OnceCell::new();
        let mut abrir = || modelo.get_or_init(|| modelos::abrir_actual().ok().flatten()).as_ref();
        ("UserPromptSubmit", recall::al_preguntar_abriendo(&mut db, &project, &e, &mut abrir)?)
    };
    if let Some(t) = texto {
        println!("{}", recall::salida_en(formato, evento, &t));
    }
    // Lo que otros equipos subieron a mitad de sesión: cada 2 minutos como mucho, aparte y sin
    // esperarlo (sin sincronización, nada).
    if !al_empezar {
        omoikane::sync::traer_si_toca(&db, &ruta);
    } else {
        // Y el índice de la documentación del proyecto, también aparte.
        omoikane::documentos::refrescar_aparte(&db, &project, &ruta);
    }
    Ok(())
}

/// La base y el modelo, con los vectores al día (si el modelo cambió o faltan, se calculan ahora).
fn abrir_con_modelo(c: &Comun) -> Result<(Connection, Option<Modelo>)> {
    let mut db = abrir(c)?;
    let modelo = modelos::abrir_actual()?;
    if let Some(m) = &modelo {
        vectores::sincronizar(&mut db, m)?;
    }
    Ok((db, modelo))
}

/// En qué estado está el modelo de significado para la base `ruta` y por qué, en JSON y en líneas para
/// `omoi doctor` y `omoi model semantic`.
fn significado_informe(ruta: &std::path::Path) -> Result<(serde_json::Value, Vec<String>)> {
    use omoikane::significado::{self as s, Porque};
    let db = if ruta.exists() { Some(db::abrir(ruta)?) } else { None };
    let e = match &db {
        Some(db) => s::estado(db),
        // Sin base aún: el entorno y la elección valen igual; si no hay ninguno, 0 memorias.
        None => match (std::env::var("OMOIKANE_SEMANTIC"), s::preferencia()) {
            (Ok(v), _) if !v.is_empty() => s::Estado { on: v != "0", reason: Porque::Env, memories: 0, threshold: s::umbral() },
            (_, Some(p)) => s::Estado { on: p, reason: Porque::Chosen, memories: 0, threshold: s::umbral() },
            _ => s::Estado { on: false, reason: Porque::Size, memories: 0, threshold: s::umbral() },
        },
    };
    let instalado = s::modelo();
    let descargando = instalado.is_none() && s::descargando();
    let vivo = if e.on && instalado.is_some() { s::vivo(std::time::Duration::from_millis(300)) } else { None };
    let (hechas, total) = match (&db, e.on && instalado.is_some()) {
        (Some(db), true) => s::cobertura(db)?,
        _ => (0, e.memories),
    };
    let (consultas, sin_respuesta, ultimo) = db.as_ref().map(omoikane::recall::consultas_al_modelo).unwrap_or((0, 0, None));
    let (n, u) = (e.memories, e.threshold);
    let porque = match e.reason {
        Porque::Env => "OMOIKANE_SEMANTIC".to_string(),
        Porque::Chosen => tr!("you turned it on", "lo encendiste tú"),
        Porque::Size => tr!("{n} memories; it turns on by itself from {u}", "{n} memorias; se enciende solo desde {u}"),
    };
    let primera = match (e.on, e.reason, &instalado) {
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
        (true, _, None) if descargando => tr!(
            "on ({porque}): downloading EmbeddingGemma (about 450 MB) in the background; until then, words only",
            "encendido ({porque}): descargando EmbeddingGemma (unos 450 MB) en segundo plano; mientras, solo palabras"
        ),
        (true, _, None) => tr!(
            "on ({porque}), but not installed: it downloads at the next session start (`omoi model install --semantic` does it now, about 450 MB)",
            "encendido ({porque}), pero sin instalar: se descarga al empezar la próxima sesión (`omoi model install --semantic` lo hace ya, unos 450 MB)"
        ),
        (true, _, Some(_)) => match vivo.as_ref().map(|r| r.pid) {
            Some(pid) => tr!(
                "on ({porque}): EmbeddingGemma, {hechas} of {total} memories with a vector; service running (pid {pid})",
                "encendido ({porque}): EmbeddingGemma, {hechas} de {total} memorias con vector; servicio en marcha (pid {pid})"
            ),
            None => tr!(
                "on ({porque}): EmbeddingGemma, {hechas} of {total} memories with a vector; the service starts with the next search",
                "encendido ({porque}): EmbeddingGemma, {hechas} de {total} memorias con vector; el servicio arranca con la próxima búsqueda"
            ),
        },
    };
    let mut lineas = vec![primera];
    if consultas > 0 {
        let ultimo = ultimo.clone().unwrap_or_default();
        lineas.push(if sin_respuesta > 0 {
            tr!(
                "recall asked it {consultas} times; {sin_respuesta} got no answer in time and went by words only, as in 1.1 (last: {ultimo})",
                "el recall le preguntó {consultas} veces; {sin_respuesta} sin respuesta a tiempo, que fueron solo por palabras, como en la 1.1 (la última: {ultimo})"
            )
        } else {
            tr!("recall asked it {consultas} times, always answered in time", "el recall le preguntó {consultas} veces, siempre respondió a tiempo")
        });
    }
    let (ram_mb, ram_origen) = s::tope_ram();
    lineas.push(ram_en_uso(ram_mb, ram_origen));
    let j = serde_json::json!({
        "on": e.on,
        "reason": e.reason,
        "memories": e.memories,
        "threshold": e.threshold,
        "choice": s::preferencia(),
        "installed": instalado.as_ref().map(|d| d.display().to_string()),
        "downloading": descargando,
        "with_vector": hechas,
        "service_pid": vivo.as_ref().map(|r| r.pid),
        "recall_asked": consultas,
        "recall_no_answer": sin_respuesta,
        "recall_last_no_answer": ultimo,
        "indexing_ram_mb": ram_mb,
        "indexing_ram_from": ram_origen,
    });
    Ok((j, lineas))
}

fn modelo(c: &Comun, accion: AccionModelo) -> Result<()> {
    match accion {
        AccionModelo::Status => {
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
            for (nombre, actual) in v {
                println!("{} {nombre}", if actual { "*" } else { " " });
            }
            println!("{}", modelos::carpeta().display());
            Ok(())
        }
        AccionModelo::Install { from, semantic: true } => {
            use omoikane::significado as s;
            let dir = s::instalar(from.as_deref(), &mut |linea| eprintln!("{linea}"))?;
            s::descarga_hecha();
            // Si esta base lo usa, que el servicio empiece ya a calcular sus vectores.
            let ruta = ruta_db(c);
            if s::en_uso_en(&ruta) {
                s::avisar(&ruta);
            }
            if c.json {
                return json(&serde_json::json!({ "semantic_model": dir.display().to_string() }));
            }
            println!("{}", tr!("meaning model ready: {}", "modelo de significado listo: {}", dir.display()));
            Ok(())
        }
        AccionModelo::Install { from, semantic: false } => {
            let id = modelos::instalar(from.as_deref(), &mut |linea| eprintln!("{linea}"))?;
            // Los vectores de la base se calculan ya, para que la primera búsqueda no los espere.
            let mut db = abrir(c)?;
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
        AccionModelo::ServeSemantic => omoikane::significado::servir(c.db.as_deref()),
        AccionModelo::IndexSemantic => {
            use omoikane::significado as s;
            let dir = s::modelo().context(tr!("the meaning model is not installed (`omoi model install --semantic`)", "el modelo de significado no está instalado (`omoi model install --semantic`)"))?;
            let gemma = omoikane::gemma::Gemma::cargar(&dir)?;
            let db = abrir(c)?;
            let (_, total) = s::cobertura(&db)?;
            let inicio = std::time::Instant::now();
            let n = s::indexar(&db, &gemma, 64, &mut || true, &mut |n| {
                if n % 100 == 0 {
                    eprintln!("{n}/{total} ({:.0} s)", inicio.elapsed().as_secs_f64());
                }
            })?;
            let (hechas, total) = s::cobertura(&db)?;
            if c.json {
                return json(&serde_json::json!({ "computed": n, "with_vector": hechas, "memories": total, "seconds": inicio.elapsed().as_secs_f64() }));
            }
            println!("{}", tr!("{n} vectors computed; {hechas} of {total} memories have one", "{n} vectores calculados; {hechas} de {total} memorias tienen el suyo"));
            Ok(())
        }
        AccionModelo::Semantic { on, off, auto, ram } => {
            use omoikane::significado as s;
            if on || off || auto {
                s::fijar(if auto { None } else { Some(on) })?;
            }
            if let Some(mb) = ram {
                s::fijar_ram((mb > 0).then_some(mb))?;
            }
            let ruta = ruta_db(c);
            let (j, lineas) = significado_informe(&ruta)?;
            // Encendido (a mano, o por el tamaño al volver a `--auto`) y sin instalar: se descarga ya.
            if (on || auto) && j["on"].as_bool() == Some(true) && s::modelo().is_none() {
                s::descargar_aparte(&ruta);
            }
            if c.json {
                return json(&j);
            }
            for l in lineas {
                println!("{l}");
            }
            Ok(())
        }
        AccionModelo::Clean => {
            let borrados = modelos::limpiar();
            if c.json {
                return json(&borrados);
            }
            println!("{}", tr!("{} models deleted", "{} modelos borrados", borrados.len()));
            Ok(())
        }
    }
}

fn abrir(c: &Comun) -> Result<Connection> {
    db::abrir(&c.db.clone().unwrap_or_else(db::ruta_por_defecto))
}

fn proyecto_de(c: &Comun) -> Result<String> {
    match &c.dir {
        Some(d) => proyecto::de(d),
        None => proyecto::actual(),
    }
}

/// Lo que se ve desde la carpeta (`--dir` o la actual), o lo que se pida (`--all`, `--project`,
/// `--space`), sin salir de su espacio.
/// `omoi decisions`: las decisiones abiertas, las bloqueantes primero.
fn decisiones_cli(c: &Comun, d: Decisiones) -> Result<()> {
    let mut db = abrir(c)?;
    if let (Some(id), Some(respuesta)) = (&d.resolve, &d.answer) {
        // Solo una que se vea desde aquí: no se resuelve la de otro cliente.
        let id = memoria::resolver_id(&db, id)?;
        anyhow::ensure!(
            Ambito::de(&db, &proyecto_de(c)?)?.ve(&db, &id)?,
            tr!("{id} is in another space: resolve it from that project", "{id} es de otro espacio: resuélvela desde ese proyecto")
        );
        let antes = omoikane::revisar::antes(&db)?;
        let m = match memoria::resolver_decision(&mut db, &id, respuesta, d.reason, d.allow_leaks)? {
            Ok(m) => m,
            Err(fugas) => {
                avisar_fugas(c, &fugas)?;
                unreachable!()
            }
        };
        let _ = omoikane::revisar::al_dia_de(&db, &antes, std::slice::from_ref(&m.id));
        omoikane::sync::programar(&ruta_db(c));
        if c.json {
            return json(&memoria::con_datos(&db, vec![m])?.remove(0));
        }
        println!("{}", tr!("resolved {}: {}", "resuelta {}: {}", corto(&m.id), m.title));
        return Ok(());
    }
    let ambito = ambito_pedido(c, &db, d.all, d.project.as_deref(), d.space.as_deref())?;
    let v = memoria::decisiones(&db, &ambito, d.blocking, &d.tags, d.limit)?;
    if c.json {
        return json(&v);
    }
    if v.is_empty() {
        println!("{}", tr!("no open decisions", "no hay decisiones abiertas"));
    }
    let con_proyecto = !matches!(ambito, Ambito::Proyecto { .. });
    for x in &v {
        let marca = if x.priority == "blocking" { tr!("BLOCKING", "BLOQUEANTE") } else { tr!("later", "después") };
        println!("{}  {:<10} {}", corto(&x.id), marca, x.title);
        let donde = if x.global {
            "global".to_string()
        } else if x.project.is_empty() {
            format!("◇ {}", x.space)
        } else {
            x.project.clone()
        };
        if con_proyecto || x.project.is_empty() {
            println!("              {donde}");
        }
        println!("              {}", una_linea(&x.content, 160));
    }
    Ok(())
}

/// `omoi file-history`: el historial de unos ficheros (los de un diff, por la entrada) para la puerta
/// de riesgo.
fn historia_ficheros_cli(c: &Comun, h: HistoriaFicheros) -> Result<()> {
    use std::io::IsTerminal;
    let mut ficheros: Vec<String> = h.files.into_iter().filter(|f| f != "-").collect();
    if ficheros.is_empty() {
        let entrada = std::io::stdin();
        anyhow::ensure!(
            !entrada.is_terminal(),
            tr!(
                "which files? Pass them as arguments, or through the standard input (git diff --name-only | omoi file-history)",
                "¿qué ficheros? Pásalos como argumentos o por la entrada estándar (git diff --name-only | omoi file-history)"
            )
        );
        let mut t = String::new();
        entrada.lock().read_to_string(&mut t)?;
        ficheros = t.lines().map(str::to_string).collect();
    }
    let project = proyecto_de(c)?;
    let db = abrir(c)?;
    let r = omoikane::riesgo::de_ficheros(&db, &project, &ficheros)?;
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
            let ids: Vec<&str> = f.gotcha_ids.iter().map(|i| corto(i)).collect();
            print!("; {} gotcha(s): {}", f.gotchas, ids.join(", "));
        }
        println!();
    }
    Ok(())
}

/// `omoi diary`.
fn diario_cli(c: &Comun, d: Diario) -> Result<()> {
    use omoikane::diario;
    let Diario { limit, since, until, file, all, project, space } = d;
    let ruta = ruta_db(c);
    if !ruta.exists() {
        if c.json {
            return json(&Vec::<diario::Linea>::new());
        }
        println!("{}", tr!("nothing yet", "nada todavía"));
        return Ok(());
    }
    let db = db::abrir(&ruta)?;
    let ambito = ambito_pedido(c, &db, all, project.as_deref(), space.as_deref())?;
    let filtro = diario::Filtro {
        desde: since.as_deref().map(|d| diario::fecha(&db, d)).transpose()?,
        hasta: until.as_deref().map(|d| diario::fecha(&db, d)).transpose()?,
        fichero: file,
        // Sin `--project`, desde la carpeta: sus líneas de antes de ser un proyecto también.
        carpeta: if project.is_none() { Some(proyecto_de(c)?) } else { None },
        limite: limit,
    };
    let v = diario::lineas(&db, &ambito, &filtro)?;
    if c.json {
        return json(&v);
    }
    if v.is_empty() {
        println!("{}", tr!("nothing yet", "nada todavía"));
    }
    let varios = !matches!(ambito, Ambito::Proyecto { .. });
    for l in &v {
        let hora = |t: &str| t.get(11..16).unwrap_or("").to_string();
        let dia = l.first_at.get(..10).unwrap_or("");
        let fin = if l.last_at.get(..10) == Some(dia) { hora(&l.last_at) } else { format!("{} {}", l.last_at.get(..10).unwrap_or(""), hora(&l.last_at)) };
        let donde = if varios { format!("  {}", proyecto::nombre_de(&l.project, "")) } else { String::new() };
        println!("{dia} {}–{fin} UTC{donde}  [{}]", hora(&l.first_at), l.session.chars().take(8).collect::<String>());
        if !l.files.is_empty() {
            println!("  {} {}", tr!("files:", "ficheros:"), l.files.join(", "));
        }
        for (que, ms) in [(tr!("recalled:", "recibidas:"), &l.recalled), (tr!("saved:", "guardadas:"), &l.saved)] {
            for m in ms.iter() {
                let corto: String = m.id.chars().take(12).collect();
                match &m.title {
                    Some(t) => println!("  {que} {corto} {t}"),
                    None => println!("  {que} {corto} {}", tr!("(forgotten)", "(olvidada)")),
                }
            }
        }
    }
    Ok(())
}

/// `omoi docs`.
fn docs_cli(c: &Comun, accion: AccionDocs) -> Result<()> {
    use omoikane::documentos as d;
    let project = proyecto_de(c)?;
    let ruta = ruta_db(c);
    // Una ruta relativa es relativa a `--dir`, si se da (como el proyecto).
    let junto_a_dir = |p: &std::path::Path| match &c.dir {
        Some(d) if p.is_relative() => d.join(p),
        _ => p.to_path_buf(),
    };
    // Mirar no crea la base.
    if !ruta.exists() && matches!(accion, AccionDocs::List | AccionDocs::Remove { .. } | AccionDocs::Refresh | AccionDocs::Test { .. }) {
        if c.json {
            return match accion {
                AccionDocs::Remove { .. } => json(&serde_json::json!({ "removed": false })),
                AccionDocs::Test { .. } => json(&None::<d::Encontrado>),
                _ => json(&Vec::<d::Carpeta>::new()),
            };
        }
        println!("{}", tr!("no documentation folders yet (`omoi docs add <folder>`)", "aún no hay carpetas de documentación (`omoi docs add <carpeta>`)"));
        return Ok(());
    }
    let db = db::abrir(&ruta)?;
    match accion {
        AccionDocs::List => {
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
        AccionDocs::Add { carpeta } => {
            let (folder, r) = d::anadir(&db, &project, &junto_a_dir(&carpeta))?;
            if c.json {
                return json(&serde_json::json!({ "folder": folder, "refresh": r }));
            }
            println!("{}", tr!("added {folder}: {} files indexed", "añadida {folder}: {} ficheros indexados", r.indexed + r.unchanged));
            if r.skipped > 0 {
                println!("{}", tr!("  {} skipped (over 2 MB, unreadable, or past {} files)", "  {} sin indexar (más de 2 MB, ilegibles o pasados {} ficheros)", r.skipped, d::MAX_FICHEROS));
            }
            Ok(())
        }
        AccionDocs::Remove { carpeta } => {
            let estaba = d::quitar(&db, &project, &junto_a_dir(std::path::Path::new(&carpeta)).to_string_lossy())?;
            if c.json {
                return json(&serde_json::json!({ "removed": estaba }));
            }
            if estaba {
                println!("{}", tr!("removed {carpeta}", "quitada {carpeta}"));
            } else {
                println!("{}", tr!("{carpeta} is not a documentation folder of this project (see `omoi docs list`)", "{carpeta} no es una carpeta de documentación de este proyecto (mira `omoi docs list`)"));
            }
            Ok(())
        }
        AccionDocs::Refresh => {
            let Some(hechos) = d::refrescar_proyecto(&db, &project)? else {
                // Otro refresco del proyecto (el del arranque de una sesión, o uno a mano) aún no acabó.
                let ocupado = tr!(
                    "the docs of this project are already being refreshed (try again in a few minutes)",
                    "ya se está poniendo al día la documentación de este proyecto (prueba en unos minutos)"
                );
                if c.json {
                    eprintln!("{ocupado}");
                    return json(&Vec::<serde_json::Value>::new());
                }
                println!("{ocupado}");
                return Ok(());
            };
            if c.json {
                return json(&hechos.iter().map(|(folder, r)| serde_json::json!({ "folder": folder, "refresh": r })).collect::<Vec<_>>());
            }
            for (folder, r) in &hechos {
                println!("{}", tr!("{folder}: {} indexed, {} removed, {} unchanged", "{folder}: {} indexados, {} quitados, {} sin cambios", r.indexed, r.removed, r.unchanged));
            }
            Ok(())
        }
        AccionDocs::Test { texto } => {
            let e = d::buscar(&db, &project, &texto, &Default::default())?;
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

fn ambito_pedido(c: &Comun, db: &Connection, todos: bool, project: Option<&str>, space: Option<&str>) -> Result<Ambito> {
    let actual = proyecto_de(c)?;
    // Sus oficios, si nunca se miraron: sin ellos no vería las memorias de su oficio. Opcional.
    if let Err(e) = omoikane::oficios::preparar(db, &actual) {
        eprintln!("omoikane: trades: {e:#}");
    }
    let base = Ambito::de(db, &actual)?;
    let pedido = if let Some(s) = space {
        Ambito::Espacio(Some(proyecto::buscar_espacio(db, s)?))
    } else if let Some(p) = project {
        let id = proyecto::buscar_en(db, p, &base)?;
        Ambito::Proyecto { id, space: proyecto::espacio_de(db, id)? }
    } else if todos {
        base.amplio()
    } else {
        base
    };
    anyhow::ensure!(
        base.permite(&pedido),
        tr!(
            "that is in another space: from this project you only see its own space; run it from that project or from your home folder",
            "eso es de otro espacio: desde este proyecto solo se ve su espacio; hazlo desde ese proyecto o desde tu carpeta de usuario"
        )
    );
    Ok(pedido)
}

/// Un proyecto en pausa sin espacio aquí entra en uno (`projects move --space`, `space assign`) solo con
/// `--force`: la pausa la puso otro equipo diciendo que es de un cliente, y aceptarlo manda sus
/// memorias al repositorio de ese cliente.
fn pausado_sin_espacio_exige_force(db: &Connection, id: i64, destino: i64, pausado: bool, force: bool) -> Result<()> {
    if pausado && !force && proyecto::espacio_de(db, id)?.is_none() {
        bail!(tr!(
            "the project is paused because another computer says it belongs to a space: moving it to «{}» sends its memories to that space's sync repository. If you are not sure, `omoi projects keep` leaves it as it is; add --force if moving it is what you want",
            "el proyecto está en pausa porque otro equipo dice que es de un espacio: pasarlo a «{}» manda sus memorias al repositorio de ese espacio. Si no lo sabes, `omoi projects keep` lo deja como está; añade --force si es lo que quieres",
            proyecto::clave_espacio(db, Some(destino))?
        ));
    }
    Ok(())
}

/// `omoi projects`.
fn proyectos_cli(c: &Comun, accion: AccionProyectos) -> Result<()> {
    let mut db = abrir(c)?;
    match accion {
        AccionProyectos::List => {
            let base = Ambito::de(&db, &proyecto_de(c)?)?;
            let filtro = if matches!(base, Ambito::Todos) { None } else { Some(base.espacio()) };
            let mut v = proyecto::lista(&db, None, filtro, usize::MAX)?;
            // Sus oficios: los de los que nunca se miraron, ahora (raíz y un nivel de cada uno).
            for p in v.iter_mut().filter(|p| !p.gone && p.trades.is_empty()) {
                p.trades = omoikane::oficios::al_dia_si_falta(&db, p.id, &p.path)?;
            }
            if c.json {
                return json(&v);
            }
            for p in &v {
                let espacio = if p.space.is_empty() { String::new() } else { format!(" [{}]", p.space) };
                let ido = if p.gone { tr!("  (folder gone)", "  (ya no está)") } else { String::new() };
                let oficios = if p.trades.is_empty() { String::new() } else { format!("  ({})", p.trades.join(", ")) };
                println!("{:>5}  {}  {}{espacio}  {}{ido}{oficios}", p.memories, p.last_update.get(..10).unwrap_or(""), p.name, p.path);
            }
            Ok(())
        }
        AccionProyectos::Move { proyecto: p, carpeta, space, force } => {
            let id = proyecto::buscar(&db, &p)?;
            // De un espacio a otro, solo con --force (como merge y move por carpeta): lo de un cliente
            // pasaría al repositorio de otro. De ninguno a uno, no hace falta, salvo si está en pausa
            // (porque otro equipo dice que es de un espacio: puede no serlo). El espacio que dice la línea
            // que pausó el proyecto se crea aquí si aún no está.
            let clave = proyecto::clave_de(&db, id)?;
            let pausa = proyecto::en_pausa(&db)?.into_iter().find(|x| x.key == clave);
            let destino = match space.as_deref() {
                Some(s) if pausa.as_ref().is_some_and(|x| !x.pausa.space.is_empty() && x.pausa.space == proyecto::clave_de_espacio(s)) => {
                    proyecto::espacio_de_clave(&db, &proyecto::clave_de_espacio(s))?
                }
                Some(s) => Some(proyecto::buscar_espacio(&db, s)?),
                None => None,
            };
            if let Some(d) = destino {
                pausado_sin_espacio_exige_force(&db, id, d, pausa.is_some(), force)?;
            }
            if destino.is_some() && proyecto::espacio_de(&db, id)?.is_none() && personal_decidido(&db, id)? && !force {
                bail!(tr!(
                    "this project was made personal on purpose (`omoi space unassign --force`): add --force to put it in a space",
                    "este proyecto se hizo personal a propósito (`omoi space unassign --force`): añade --force para meterlo en un espacio"
                ));
            }
            if let (Some(antes), Some(d)) = (proyecto::espacio_de(&db, id)?, destino)
                && antes != d
                && !force
            {
                bail!(tr!(
                    "the project is in the space «{}»: moving it to «{}» sends its memories to that space's sync repository. Add --force if that is what you want",
                    "el proyecto está en el espacio «{}»: moverlo a «{}» manda sus memorias al repositorio de ese espacio. Añade --force si es lo que quieres",
                    proyecto::clave_espacio(&db, Some(antes))?,
                    proyecto::clave_espacio(&db, Some(d))?
                ));
            }
            if let Some(carpeta) = carpeta {
                let nueva = proyecto::de(&carpeta)?;
                let fundidas = proyecto::mover(&mut db, id, &nueva, force)?;
                if fundidas > 0 {
                    println!("{}", tr!("{nueva}: moved there (its {fundidas} memories joined it)", "{nueva}: movido allí (se le han sumado sus {fundidas} memorias)"));
                } else {
                    println!("{}", tr!("moved to {nueva}", "movido a {nueva}"));
                }
            }
            if let (Some(s), Some(d)) = (space, destino) {
                proyecto::asignar_espacio(&db, id, Some(d))?;
                db.execute("DELETE FROM meta WHERE k = 'personal:' || ?1", [id])?;
                println!("{}", tr!("in the space «{s}» now", "ahora en el espacio «{s}»"));
                if pausa.is_some() {
                    println!("{}", tr!("it is no longer paused: it syncs again", "ya no está en pausa: vuelve a sincronizarse"));
                }
            }
            Ok(())
        }
        AccionProyectos::Keep { proyecto: p } => {
            let id = proyecto::buscar(&db, &p)?;
            match proyecto::quedarse(&db, id)? {
                None => println!("{}", tr!("it is not paused: nothing to do", "no está en pausa: nada que hacer")),
                Some(e) if e.is_empty() => println!("{}", tr!("it stays in no space and syncs again", "se queda sin espacio y vuelve a sincronizarse")),
                Some(e) => println!("{}", tr!("it stays in the space «{e}» and syncs again", "se queda en el espacio «{e}» y vuelve a sincronizarse")),
            }
            omoikane::sync::programar(&ruta_db(c));
            Ok(())
        }
        AccionProyectos::Prune => {
            let n = proyecto::podar(&db)?;
            println!("{}", tr!("{n} projects without memories forgotten", "{n} proyectos sin memorias olvidados"));
            Ok(())
        }
        AccionProyectos::Distinct { a, b } => {
            let de = proyecto::buscar(&db, &a)?;
            let otro = b.as_deref().map(|b| proyecto::buscar(&db, b)).transpose()?;
            omoikane::nombres::marcar_distintos(&db, de, otro)?;
            println!("{}", tr!("ok: the review won't propose it again", "hecho: la revisión no volverá a proponerlo"));
            Ok(())
        }
        AccionProyectos::Merge { a, b, force } => {
            let (de, en) = (proyecto::buscar(&db, &a)?, proyecto::buscar(&db, &b)?);
            let n = proyecto::fundir(&mut db, de, en, force)?;
            println!("{}", tr!("merged: {n} memories moved to {b}", "fundidos: {n} memorias pasan a {b}"));
            Ok(())
        }
    }
}

/// `omoi space`.
fn espacios_cli(c: &Comun, accion: AccionEspacio) -> Result<()> {
    let db = abrir(c)?;
    let elegido = |p: Option<String>| -> Result<i64> {
        match p {
            Some(p) => proyecto::buscar(&db, &p),
            None => proyecto::registrar(&db, &proyecto_de(c)?),
        }
    };
    match accion {
        AccionEspacio::List => {
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
        AccionEspacio::Add { nombre } => {
            proyecto::crear_espacio(&db, &nombre)?;
            println!("{}", tr!("space «{}» created; put projects in it with `omoi space assign {}`", "espacio «{}» creado; mete proyectos con `omoi space assign {}`", proyecto::clave_de_espacio(&nombre), proyecto::clave_de_espacio(&nombre)));
            Ok(())
        }
        AccionEspacio::Assign { espacio, proyecto: p, force } => {
            let s = proyecto::buscar_espacio(&db, &espacio)?;
            let id = elegido(p)?;
            let pausado = proyecto::pausado(&db, &proyecto::clave_de(&db, id)?)?;
            pausado_sin_espacio_exige_force(&db, id, s, pausado, force)?;
            // De un espacio a otro, solo con --force, como `projects move --space`: lo de un cliente
            // pasaría al repositorio de otro.
            if let Some(antes) = proyecto::espacio_de(&db, id)?
                && antes != s
                && !force
            {
                bail!(tr!(
                    "the project is in the space «{}»: moving it to «{}» sends its memories to that space's sync repository. Add --force if that is what you want",
                    "el proyecto está en el espacio «{}»: moverlo a «{}» manda sus memorias al repositorio de ese espacio. Añade --force si es lo que quieres",
                    proyecto::clave_espacio(&db, Some(antes))?,
                    proyecto::clave_espacio(&db, Some(s))?
                ));
            }
            if personal_decidido(&db, id)? && !force {
                bail!(tr!(
                    "this project was made personal on purpose (`omoi space unassign --force`): add --force to put it in «{espacio}»",
                    "este proyecto se hizo personal a propósito (`omoi space unassign --force`): añade --force para meterlo en «{espacio}»"
                ));
            }
            proyecto::asignar_espacio(&db, id, Some(s))?;
            db.execute("DELETE FROM meta WHERE k = 'personal:' || ?1", [id])?;
            println!("{}", tr!("done: its memories are now seen only from the space «{espacio}» (and the home folder)", "hecho: sus memorias ya solo se ven desde el espacio «{espacio}» (y desde la carpeta de usuario)"));
            Ok(())
        }
        AccionEspacio::Unassign { proyecto: p, force } => {
            let id = elegido(p)?;
            // Sacarlo de su espacio manda sus memorias al repositorio general, el personal: lo de un
            // cliente acabaría ahí. Solo con --force.
            if let Some(antes) = proyecto::espacio_de(&db, id)?
                && !force
            {
                bail!(tr!(
                    "the project is in the space «{}»: without a space, its memories go to the general (personal) sync repository. Add --force if that is what you want",
                    "el proyecto está en el espacio «{}»: sin espacio, sus memorias van al repositorio general (el personal). Añade --force si es lo que quieres",
                    proyecto::clave_espacio(&db, Some(antes))?
                ));
            }
            // Lo que llegó de otros equipos por el repositorio del espacio es del cliente, no de esta
            // persona: se queda en el espacio, como memoria de espacio (sigue yendo solo a su
            // repositorio y ya no se ve desde este proyecto). Lo escrito aquí sale con el proyecto.
            let mut quedan = 0;
            if let Some(antes) = proyecto::espacio_de(&db, id)? {
                quedan = db.execute(
                    "UPDATE memory SET project_id = 0, space_id = ?2 WHERE project_id = ?1 AND source LIKE 'sync:%'",
                    rusqlite::params![id, antes],
                )?;
            }
            proyecto::asignar_espacio(&db, id, None)?;
            // La persona ha decidido que es personal: volver a meterlo en un espacio pedirá --force,
            // aunque un repositorio de cliente lo siga proponiendo.
            db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('personal:' || ?1, '1')", [id])?;
            println!("{}", tr!("done: the project is no longer in a space", "hecho: el proyecto ya no está en ningún espacio"));
            if quedan > 0 {
                println!(
                    "{}",
                    tr!(
                        "{quedan} memories that came from other computers through the space's repository stay in the space (they are the client's)",
                        "{quedan} memorias que llegaron de otros equipos por el repositorio del espacio se quedan en el espacio (son del cliente)"
                    )
                );
            }
            Ok(())
        }
    }
}

/// Los oficios de `remember` (`--trade`, o etiquetas `trade:` puestas a mano), validados y sin
/// repetir, y las etiquetas sin las de oficio.
fn oficios_pedidos(trades: &[String], tags: Vec<String>) -> Result<(Vec<String>, Vec<String>)> {
    let mut v: Vec<String> = Vec::new();
    let a_mano: Vec<String> = tags.iter().filter_map(|t| t.trim().to_lowercase().strip_prefix(omoikane::oficios::PREFIJO).map(String::from)).collect();
    for t in trades.iter().chain(&a_mano) {
        for o in omoikane::oficios::lista(t)? {
            if !v.contains(&o) {
                v.push(o);
            }
        }
    }
    let tags = tags.into_iter().filter(|t| !t.trim().to_lowercase().starts_with(omoikane::oficios::PREFIJO)).collect();
    Ok((v, tags))
}

/// Si una memoria de oficio nombra algo de un proyecto o de un cliente, lo dice y sale con código 2
/// (como una duplicada): no se guarda (salvo los falsos positivos que deje pasar `--allow-leaks`).
fn avisar_fugas(c: &Comun, fugas: &[omoikane::oficios::Fuga]) -> Result<()> {
    if fugas.is_empty() {
        return Ok(());
    }
    if c.json {
        json(&serde_json::json!({ "saved": false, "trade_leaks": fugas }))?;
    } else {
        println!(
            "{}",
            tr!(
                "not saved: a trade memory reaches every project of that trade, of every client, and this one names what looks like a project or a client:",
                "no guardada: una memoria de oficio llega a todos los proyectos de ese oficio, de todos los clientes, y esta nombra lo que parece de un proyecto o de un cliente:"
            )
        );
        for f in fugas {
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

/// Los oficios de `update` (ver `oficios::al_cambiar`): `--trade` la hace de oficio (la promueve),
/// `--no-trade` deja de serlo, `--for` dice para quién; y lo nuevo de una memoria de oficio (texto,
/// rutas, etiquetas, datos) pasa por `oficios::fugas` (salvo lo que deje pasar `--allow-leaks`).
/// Devuelve los cambios y, si es de un cliente y se promueve, la memoria del cliente: entonces no se
/// cambia, se crea una de oficio nueva (`oficios::crear_de_oficio`).
#[allow(clippy::too_many_arguments)]
fn oficios_al_cambiar(
    c: &Comun,
    db: &Connection,
    id: &str,
    mut cambios: Cambios,
    trades: &[String],
    no_trade: bool,
    para: Option<&str>,
    datos: Option<&Option<serde_json::Value>>,
    allow_leaks: bool,
) -> Result<(Cambios, Option<Memoria>)> {
    use omoikane::oficios;
    let actual = memoria::obtener(db, id)?;
    let trade = if no_trade { Some(String::new()) } else { (!trades.is_empty()).then(|| trades.join(",")) };
    let pedido = oficios::PedidoCambio { trade: trade.as_deref(), para, cambios: &cambios, datos: datos.map(|d| d.as_ref()), permitir_fugas: allow_leaks };
    let plan = match oficios::al_cambiar(db, &actual, pedido)? {
        Ok(plan) => plan,
        Err(fugas) => {
            avisar_fugas(c, &fugas)?;
            unreachable!()
        }
    };
    cambios.tags = plan.tags;
    if plan.nueva {
        return Ok((cambios, Some(actual)));
    }
    if plan.hacer_global {
        cambios.project = Some(String::new());
    }
    Ok((cambios, None))
}

/// `omoi trades`.
fn oficios_cli(c: &Comun, accion: AccionOficios) -> Result<()> {
    use omoikane::oficios;
    let db = abrir(c)?;
    // El proyecto pedido (o el de esta carpeta, apuntándolo si aún no lo está) y su carpeta.
    let elegido = |p: Option<String>| -> Result<(i64, String)> {
        match p {
            Some(p) => {
                let id = proyecto::buscar(&db, &p)?;
                let ruta: String = db.query_row("SELECT path FROM project WHERE id = ?1", [id], |f| f.get(0))?;
                Ok((id, ruta))
            }
            None => {
                let ruta = proyecto_de(c)?;
                anyhow::ensure!(
                    !proyecto::es_general(&ruta),
                    tr!("this folder is not a project: run it in one, or give --project", "esta carpeta no es un proyecto: hazlo en uno, o di --project")
                );
                Ok((proyecto::registrar(&db, &ruta)?, ruta))
            }
        }
    };
    let nombres = |v: &[String]| -> Result<Vec<String>> { v.iter().map(|o| oficios::nombre(o)).collect() };
    let decir = |id: i64| -> Result<()> {
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
    match accion {
        AccionOficios::List { all: false } => {
            let (id, ruta) = elegido(None)?;
            oficios::al_dia(&db, id, &ruta)?;
            decir(id)
        }
        AccionOficios::List { all: true } => {
            let base = Ambito::de(&db, &proyecto_de(c)?)?;
            let filtro = if matches!(base, Ambito::Todos) { None } else { Some(base.espacio()) };
            let mut v = proyecto::lista(&db, None, filtro, usize::MAX)?;
            for p in v.iter_mut().filter(|p| !p.gone) {
                p.trades = oficios::al_dia_si_falta(&db, p.id, &p.path)?;
            }
            let por_oficio = oficios::memorias_por_oficio(&db)?;
            if c.json {
                let proyectos: Vec<_> = v.iter().map(|p| serde_json::json!({ "name": p.name, "path": p.path, "trades": p.trades })).collect();
                return json(&serde_json::json!({ "projects": proyectos, "memories": por_oficio }));
            }
            for p in &v {
                println!("{:<28}  {}", p.name, if p.trades.is_empty() { "-".to_string() } else { p.trades.join(", ") });
            }
            if !por_oficio.is_empty() {
                let mut cuentas: Vec<_> = por_oficio.into_iter().collect();
                cuentas.sort();
                println!("\n{}", tr!("trade memories: {}", "memorias de oficio: {}", cuentas.iter().map(|(o, n)| format!("{o} {n}")).collect::<Vec<_>>().join(", ")));
            }
            Ok(())
        }
        AccionOficios::Add { oficios: os, project } => {
            let (id, _) = elegido(project)?;
            oficios::anadir(&db, id, &nombres(&os)?)?;
            decir(id)
        }
        AccionOficios::Remove { oficios: os, project } => {
            let (id, _) = elegido(project)?;
            oficios::quitar(&db, id, &nombres(&os)?)?;
            decir(id)
        }
        AccionOficios::Set { oficios: os, project } => {
            let (id, _) = elegido(project)?;
            oficios::fijar(&db, id, &nombres(&os)?)?;
            decir(id)
        }
        AccionOficios::Auto { project } => {
            let (id, ruta) = elegido(project)?;
            oficios::automatico(&db, id, &ruta)?;
            decir(id)
        }
    }
}

/// Si la persona sacó el proyecto de su espacio a propósito (`space unassign --force`).
fn personal_decidido(db: &Connection, id: i64) -> Result<bool> {
    Ok(db.query_row("SELECT count(*) FROM meta WHERE k = 'personal:' || ?1", [id], |f| f.get::<_, i64>(0))? > 0)
}

fn texto_o_entrada(t: Option<String>) -> Result<String> {
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

/// Por stderr (también con `--json`): lo guardado no es lo que se escribió.
/// Si el cambio le quitó la verificación de una persona: que lo sepa quien cambia.
fn avisar_desverificada(m: &Memoria) {
    if let Some(v) = &m.unverified {
        eprintln!(
            "{}",
            tr!(
                "its text changed, so it is no longer verified (it was by {}): `omoi verify {}` if it is still right",
                "su texto ha cambiado, así que ya no está verificada (lo estaba por {}): `omoi verify {}` si sigue siendo correcta",
                v.by,
                corto(&m.id)
            )
        );
    }
}

/// `omoi verify`: solo una persona. En una terminal pregunta; fuera, pide `--yes`; dentro de la shell
/// de un agente se niega siempre (ver `verificado`).
fn verificar_cli(c: &Comun, v: Verificar) -> Result<()> {
    if let Some(var) = omoikane::verificado::shell_de_agente() {
        bail!(tr!(
            "verifying is for a person, and this looks like a coding agent's shell ({var} is set): run it in your own terminal, or use the web view (`omoi serve --open`)",
            "verificar es cosa de una persona, y esto parece la shell de un agente ({var} está puesta): hazlo en tu terminal, o en la vista web (`omoi serve --open`)"
        ));
    }
    let mut db = abrir(c)?;
    let m = memoria::obtener(&db, &v.id)?;
    if v.unverify {
        let habia = omoikane::verificado::desverificar(&mut db, &m.id)?;
        if c.json {
            return json(&serde_json::json!({ "id": m.id, "verified": false, "changed": habia }));
        }
        println!("{}", if habia { tr!("{} is no longer verified", "{} ya no está verificada", corto(&m.id)) } else { tr!("{} was not verified", "{} no estaba verificada", corto(&m.id)) });
        return Ok(());
    }
    let by = match v.by {
        Some(b) => b,
        None => omoikane::verificado::quien_por_defecto(&db, &m)?,
    };
    if !v.yes {
        if !omoikane::entorno::puede_preguntar() {
            bail!(tr!("add --yes to verify {} without asking", "añade --yes para verificar {} sin preguntar", corto(&m.id)));
        }
        println!("{}  {}\n{}\n\n{}\n", corto(&m.id), m.kind, m.title, m.content);
        let quien = if by.trim().is_empty() { tr!("(no name: give --by)", "(sin nombre: di --by)") } else { by.clone() };
        if !preguntar_si(&tr!("Verify it as {quien}? You vouch that this text is right.", "¿Verificarla como {quien}? Respondes de que este texto es correcto."), false)? {
            println!("{}", tr!("nothing changed", "no se ha cambiado nada"));
            return Ok(());
        }
    }
    let m = omoikane::verificado::verificar(&mut db, &m.id, &by)?;
    if c.json {
        return json(&m);
    }
    let quien = m.verified.as_ref().map(|x| x.by.clone()).unwrap_or(by);
    println!("{}", tr!("verified {} by {quien}", "verificada {} por {quien}", corto(&m.id)));
    Ok(())
}

fn avisar_redactados(m: &Memoria) {
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

/// Si un `update` no cambia nada de la memoria (solo, quizá, sus datos).
fn sin_cambios(c: &Cambios) -> bool {
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

/// Lo que se tapó en los datos por parecer un secreto, a stderr.
fn avisar_datos_tapados(tipos: &[&str]) {
    if !tipos.is_empty() {
        eprintln!(
            "{}",
            tr!(
                "omoikane: what looked like a secret in the data was replaced by [redacted:<type>] ({})",
                "omoikane: lo que parecía un secreto en los datos se cambió por [redacted:<tipo>] ({})",
                tipos.join(", ")
            )
        );
    }
}

fn json(v: &impl serde::Serialize) -> Result<()> {
    let mut salida = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut salida, v)?;
    writeln!(salida)?;
    Ok(())
}

/// Lo que enseña la CLI del id: 10 caracteres de tiempo y 2 aleatorios. `get` acepta este principio.
fn corto(id: &str) -> &str {
    id.char_indices().nth(12).map_or(id, |(i, _)| &id[..i])
}

fn una_linea(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max { s } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

fn lista(m: &Memoria, con_proyecto: bool) {
    let fijada = if m.pinned { tr!(" (pinned)", " (fijada)") } else { String::new() };
    let verificada = m.verified.as_ref().map(|v| tr!(" (verified by {})", " (verificada por {})", v.by)).unwrap_or_default();
    println!("{}  {:<10} {}{fijada}{verificada}", corto(&m.id), m.kind.as_str(), m.title);
    let mut extra: Vec<String> = m.paths.clone();
    extra.extend(m.tags.iter().filter(|t| !t.starts_with(omoikane::oficios::PREFIJO) && !t.starts_with(omoikane::oficios::PARA)).map(|t| format!("#{t}")));
    let oficios = omoikane::oficios::de_memoria(m);
    if !oficios.is_empty() {
        let para = omoikane::oficios::para_de(&m.tags).map(|p| format!(" ({p})")).unwrap_or_default();
        extra.push(format!("trade {}{para}", oficios.join(", ")));
    } else if m.project.is_empty() {
        extra.push("global".into());
    } else if con_proyecto {
        extra.push(m.project.clone());
    }
    if !extra.is_empty() {
        println!("              {}", extra.join(" · "));
    }
    println!("              {}", una_linea(&m.content, 160));
}

fn entera(m: &Memoria) {
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
    let oficios = omoikane::oficios::de_memoria(m);
    let proyecto = if !oficios.is_empty() {
        format!("trade {}", oficios.join(", "))
    } else if m.project.is_empty() {
        "global".to_string()
    } else {
        m.project.clone()
    };
    println!("{}{proyecto}", tr!("project    ", "proyecto   "));
    println!("{}{}", tr!("source     ", "origen     "), m.source);
    if let Some(p) = &m.provenance {
        println!("{}{}", tr!("written in ", "escrita en "), p.texto());
    }
    match &m.verified {
        Some(v) => println!("{}", tr!("verified   by {} ({})", "verificada por {} ({})", v.by, v.at)),
        None => println!("{}", tr!("verified   no: written by an agent, or changed since", "verificada no: la escribió un agente, o cambió después")),
    }
    println!("{}", tr!("created    {}   changed {}", "creada     {}   cambiada {}", m.created_at, m.updated_at));
    if m.recall_count > 0 {
        let ultima = m.recalled_at.as_deref().unwrap_or("-");
        let n = m.recall_count;
        let veces = if n == 1 { tr!("once", "1 vez") } else { tr!("{n} times", "{n} veces") };
        println!("{}", tr!("recalled   {veces} (last {ultima})", "recuperada {veces} (la última, {ultima})"));
    }
}

fn historial(m: &Memoria, v: &[Version]) {
    println!("{}  {}", corto(&m.id), m.title);
    if let Some(p) = &m.provenance {
        println!("{}", tr!("written in {} ({})", "escrita en {} ({})", p.texto(), m.created_at));
    }
    if let Some(x) = &m.verified {
        println!("{}", tr!("verified by {} ({})", "verificada por {} ({})", x.by, x.at));
    }
    if v.is_empty() {
        println!("{}", tr!("no previous versions", "sin versiones anteriores"));
        return;
    }
    for x in v {
        let motivo = x.reason.as_deref().map(|r| format!(" · {r}")).unwrap_or_default();
        println!("\n{}", tr!("v{}  until {}{motivo}", "v{}  hasta {}{motivo}", x.version, x.changed_at));
        println!("  {}  {}", x.kind, x.title);
        println!("  {}", una_linea(&x.content, 300));
    }
}

/// «indexing RAM: 64 MB (default)…», para `omoi model semantic` y `omoi doctor`.
fn ram_en_uso(mb: u64, origen: omoikane::significado::OrigenRam) -> String {
    use omoikane::significado::{OrigenRam, RAM_MINIMA_MB};
    let de = match origen {
        OrigenRam::Entorno => tr!("from OMOIKANE_SEMANTIC_RAM", "de OMOIKANE_SEMANTIC_RAM"),
        OrigenRam::Elegido => tr!("chosen with `omoi model semantic --ram`", "elegido con `omoi model semantic --ram`"),
        OrigenRam::PorDefecto => tr!("default; `omoi model semantic --ram <MB>` changes it", "por defecto; `omoi model semantic --ram <MB>` lo cambia"),
    };
    tr!("indexing RAM: up to {mb} MB ({de}; minimum {RAM_MINIMA_MB})", "RAM al indexar: hasta {mb} MB ({de}; mínimo {RAM_MINIMA_MB})")
}

fn doctor(c: &Comun) -> Result<()> {
    db::registrar_sqlite_vec();
    let prueba = Connection::open_in_memory()?;
    let sqlite: String = prueba.query_row("select sqlite_version()", [], |r| r.get(0))?;
    let vec: String = prueba.query_row("select vec_version()", [], |r| r.get(0))?;
    prueba.execute_batch("create virtual table t using fts5(x, tokenize='unicode61 remove_diacritics 2');")?;
    prueba.execute_batch("create virtual table v using vec0(e float[4]);")?;
    println!("sqlite     {sqlite}");
    println!("sqlite-vec {vec}");
    println!("{}", tr!("fts5       ok (unicode61, accent-insensitive)", "fts5       ok (unicode61, sin acentos)"));
    println!("vec0       ok");
    let ruta = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
    if ruta.exists() {
        let base = db::abrir(&ruta)?;
        let n: i64 = base.query_row("SELECT count(*) FROM memory", [], |f| f.get(0))?;
        let p: i64 = base.query_row("SELECT count(DISTINCT project_id) FROM memory WHERE project_id != 0", [], |f| f.get(0))?;
        let v = db::VERSION_ESQUEMA;
        let r = ruta.display();
        let memorias = if n == 1 { tr!("1 memory", "1 memoria") } else { tr!("{n} memories", "{n} memorias") };
        let proyectos = if p == 1 { tr!("1 project", "1 proyecto") } else { tr!("{p} projects", "{p} proyectos") };
        println!("{}", tr!("database   {r} ({memorias}, {proyectos}, schema {v})", "base       {r} ({memorias}, {proyectos}, esquema {v})"));
        for x in proyecto::lista(&base, None, None, usize::MAX)?.into_iter().filter(|x| x.gone) {
            println!("{}", tr!("project    {} is gone from {}: `omoi projects move {} <new folder>`", "proyecto   {} ya no está en {}: `omoi projects move {} <carpeta nueva>`", x.name, x.path, x.name));
        }
        for a in omoikane::sync::avisos_de_pausa(&base, &ruta) {
            println!("{}{a}", tr!("paused     ", "en pausa   "));
        }
        let papelera: i64 = base.query_row("SELECT count(*) FROM forgotten", [], |f| f.get(0))?;
        let copias = omoikane::copia::copias(&ruta);
        let total_kb: u64 = copias.iter().map(|(_, b)| b).sum::<u64>() / 1024;
        match copias.first() {
            Some((p, _)) => {
                let nombre = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                println!("{}", tr!("backups    {} ({} copies, {total_kb} KB, {} in the trash)", "copias     {} ({} copias, {total_kb} KB, {} en la papelera)", nombre, copias.len(), papelera));
            }
            None if omoikane::copia::copias_a_conservar() == 0 => println!("{}", tr!("backups    off (OMOIKANE_BACKUPS=0)", "copias     apagadas (OMOIKANE_BACKUPS=0)")),
            None => println!("{}", tr!("backups    none yet (the first one happens at the next session start)", "copias     todavía ninguna (la primera, al empezar la próxima sesión)")),
        }
        let con_secreto = memoria::recientes(&base, &Ambito::Todos, None, usize::MAX >> 1)?
            .iter()
            .filter(|m| !omoikane::revisar::secretos_de(m).is_empty())
            .count();
        let redaccion = if omoikane::secretos::activo() {
            tr!("on", "activa")
        } else {
            tr!("OFF (OMOIKANE_REDACT=0): secrets are saved as written", "APAGADA (OMOIKANE_REDACT=0): los secretos se guardan tal cual")
        };
        match con_secreto {
            0 => println!("{}", tr!("secrets    redaction {redaccion}; no memory looks like it holds one", "secretos   redacción {redaccion}; ninguna memoria parece llevar uno")),
            1 => println!(
                "{}",
                tr!(
                    "secrets    redaction {redaccion}; 1 memory looks like it holds one (`omoi review`, or Redact in the web view)",
                    "secretos   redacción {redaccion}; 1 memoria parece llevar uno (`omoi review`, o Redactar en la vista web)"
                )
            ),
            n => println!(
                "{}",
                tr!(
                    "secrets    redaction {redaccion}; {n} memories look like they hold one (`omoi review`, or Redact in the web view)",
                    "secretos   redacción {redaccion}; {n} memorias parecen llevar uno (`omoi review`, o Redactar en la vista web)"
                )
            ),
        }
    } else {
        let r = ruta.display();
        println!("{}", tr!("database   {r} (not created yet: it appears with the first memory)", "base       {r} (todavía no existe: se crea al guardar la primera memoria)"));
        if !omoikane::secretos::activo() {
            println!("{}", tr!("secrets    redaction OFF (OMOIKANE_REDACT=0): secrets are saved as written", "secretos   redacción APAGADA (OMOIKANE_REDACT=0): los secretos se guardan tal cual"));
        }
    }
    match modelos::actual() {
        Some(d) => {
            let nombre = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            println!("{}{nombre}", tr!("model      ", "modelo     "));
            if ruta.exists() {
                let (de, n) = vectores::estado(&db::abrir(&ruta)?)?;
                let al_dia = de.as_deref() == Some(nombre.as_str());
                println!(
                    "{}",
                    if al_dia {
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
    for (k, l) in significado_informe(&ruta)?.1.into_iter().enumerate() {
        println!("{}{l}", if k == 0 { tr!("meaning    ", "significado ") } else { "           ".to_string() });
    }
    if let Ok(p) = proyecto_de(c) {
        println!("{}{p}", tr!("project    ", "proyecto   "));
    }
    let ruta = ruta_db(c);
    println!("{}{}", tr!("sync       ", "sincroniz. "), estado_sync(&ruta, None));
    for e in omoikane::sync::espacios(&ruta) {
        println!("{}{}", tr!("  space {e}: ", "  espacio {e}: "), estado_sync(&ruta, Some(&e)));
    }
    println!("{}", tr!("language   en (OMOIKANE_LANG=es for Spanish)", "idioma     es (OMOIKANE_LANG)"));
    Ok(())
}

/// Una línea con el estado de la sincronización de un repositorio, el general o el de un espacio
/// (para `doctor` y `sync status`).
fn estado_sync(ruta: &std::path::Path, espacio: Option<&str>) -> String {
    use omoikane::sync;
    let r = sync::rutas_de(ruta, espacio);
    let Ok(Some(config)) = sync::leer_config_de(&r) else {
        return tr!("off (`omoi sync init <git url>` to share memories between computers)", "apagada (`omoi sync init <url de git>` para compartir memorias entre equipos)");
    };
    let e = sync::leer_estado_de(&r);
    let hace = |s: &str| {
        let t = s.parse::<u64>().unwrap_or(0);
        let ahora = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let m = ahora.saturating_sub(t) / 60;
        if m < 60 { tr!("{m} min ago", "hace {m} min") } else { tr!("{} h ago", "hace {} h", m / 60) }
    };
    let mut l = sync::sin_credenciales(&config.url);
    match (&e.ultimo_error, &e.ultimo_ok) {
        (Some(err), _) => l.push_str(&tr!(" · FAILING: {}", " · FALLA: {}", sync::sin_credenciales(err.lines().next().unwrap_or_default()))),
        (None, Some(ok)) => l.push_str(&tr!(" · last ok {}", " · última bien {}", hace(ok))),
        (None, None) => l.push_str(&tr!(" · not synced yet", " · aún sin sincronizar")),
    }
    if sync::pendiente(ruta) {
        l.push_str(&tr!(" · changes pending", " · cambios pendientes"));
    }
    if let Some(a) = e.aviso {
        l.push_str(&format!(" · {a}"));
    }
    if sync::clave_cambiada(&r) {
        l.push_str(&format!(" · {}", sync::aviso_clave_cambiada()));
    }
    // Lo dice aunque el error de la última vez ya lo diga: es lo que hay que hacer.
    if sync::necesita_version(&r).is_some() && !e.ultimo_error.as_deref().is_some_and(|x| x.contains(&sync::aviso_version())) {
        l.push_str(&format!(" · {}", sync::aviso_version()));
    }
    l
}

/// El espacio de una opción `--space`, ya comprobado.
fn espacio_de(space: Option<String>) -> Result<Option<String>> {
    space.map(|e| omoikane::sync::espacio_valido(&e)).transpose()
}

/// Las claves de un repositorio para `sync status`: «k1, k2 (vigente k2)».
fn claves_sync(r: &omoikane::sync::Rutas) -> String {
    let (ids, vigente) = omoikane::sync::claves_de(r);
    let lista: Vec<String> = ids.iter().map(|i| format!("k{i}")).collect();
    let vigente = if vigente == 0 { ids.last().copied().unwrap_or(1) } else { vigente };
    tr!("{} (current: k{})", "{} (vigente: k{})", lista.join(", "), vigente)
}

fn preguntar_si(texto: &str, defecto: bool) -> Result<bool> {
    let opciones = if defecto { tr!("[Y/n]", "[S/n]") } else { tr!("[y/N]", "[s/N]") };
    print!("{texto} {opciones} ");
    std::io::stdout().flush()?;
    let mut l = String::new();
    std::io::stdin().read_line(&mut l)?;
    let l = l.trim().to_lowercase();
    Ok(if l.is_empty() { defecto } else { matches!(l.as_str(), "s" | "si" | "sí" | "y" | "yes") })
}

/// Enseña qué se subirá y, en una terminal, pide confirmación. Sin terminal hace falta `--yes` (un
/// agente que lo lanza no debe subir memorias sin que nadie lo haya decidido).
/// `omoi sync init --create <nombre>`: crea el repositorio privado con `gh` o, si no está, con `glab`,
/// y devuelve su URL. Usa la sesión que ya tenga el usuario en esa herramienta.
fn crear_repositorio(nombre: &str) -> Result<String> {
    let probar = |cmd: &str| std::process::Command::new(cmd).arg("--version").output().is_ok_and(|o| o.status.success());
    let (cmd, args): (&str, Vec<&str>) = if probar("gh") {
        ("gh", vec!["repo", "create", nombre, "--private", "--description", "Omoikane: shared memory (encrypted)"])
    } else if probar("glab") {
        ("glab", vec!["repo", "create", nombre, "--private", "--description", "Omoikane: shared memory (encrypted)"])
    } else {
        bail!(tr!(
            "--create needs the GitHub (`gh`) or GitLab (`glab`) command line; create a private repository yourself and pass its URL",
            "--create necesita la línea de órdenes de GitHub (`gh`) o de GitLab (`glab`); crea tú un repositorio privado y pasa su URL"
        ));
    };
    let o = std::process::Command::new(cmd).args(&args).stdin(std::process::Stdio::null()).output()?;
    let texto = format!("{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    if !o.status.success() {
        bail!("{cmd} repo create: {}", texto.trim());
    }
    let url = texto
        .split_whitespace()
        .find(|p| p.starts_with("https://"))
        .map(|u| format!("{}.git", u.trim_end_matches(['/', '.'])))
        .ok_or_else(|| anyhow::anyhow!("{cmd} repo create: no URL in its output: {}", texto.trim()))?;
    eprintln!("{}", tr!("Created the private repository {url}", "Creado el repositorio privado {url}"));
    Ok(url)
}

fn confirmar_subida(db: &Connection, config: &omoikane::sync::Config, e: &omoikane::sync::Eleccion, si: bool) -> Result<bool> {
    let (g, p, np) = omoikane::sync::resumen_de_subida(db, config)?;
    println!(
        "{}",
        tr!(
            "Will upload {g} global memories and {p} from {np} project(s), encrypted (preferences: {}). Everything stays on this computer too.",
            "Se subirán {g} memorias globales y {p} de {np} proyecto(s), cifradas (preferencias: {}). Todo sigue también en este equipo.",
            if e.preferencias { tr!("yes", "sí") } else { "no".into() }
        )
    );
    if si {
        return Ok(true);
    }
    anyhow::ensure!(
        omoikane::entorno::puede_preguntar(),
        tr!("add --yes to confirm (or run it in a terminal to choose what to share)", "añade --yes para confirmarlo (o ejecútalo en una terminal para elegir qué compartir)")
    );
    if !preguntar_si(&tr!("Continue?", "¿Seguir?"), true)? {
        println!("{}", tr!("nothing changed", "no se ha cambiado nada"));
        return Ok(false);
    }
    Ok(true)
}

/// Qué compartir: lo de las opciones y, en una terminal sin `--yes`, lo que conteste el usuario.
fn elegir(o: &OpcionesSync) -> Result<omoikane::sync::Eleccion> {
    let proyectos = o.project.iter().map(|p| proyecto::de(p)).collect::<Result<Vec<_>>>()?;
    let mut e = omoikane::sync::Eleccion {
        preferencias: o.share_preferences,
        global: !o.no_global,
        proyectos,
        espacios: Vec::new(),
        autor: if o.no_author { None } else { o.author.clone().or_else(omoikane::entorno::autor).or_else(omoikane::sync::nombre_git) },
    };
    if o.yes || !omoikane::entorno::puede_preguntar() {
        return Ok(e);
    }
    e.preferencias = preguntar_si(&tr!("Share your preferences too? They are usually personal.", "¿Compartir también tus preferencias? Suelen ser personales."), e.preferencias)?;
    e.global = preguntar_si(&tr!("Share your global memories?", "¿Compartir tus memorias globales?"), e.global)?;
    if e.proyectos.is_empty() {
        print!("{} ", tr!("Projects to share (Enter = all; or folders separated by commas):", "Proyectos que compartir (Intro = todos; o carpetas separadas por comas):"));
        std::io::stdout().flush()?;
        let mut l = String::new();
        std::io::stdin().read_line(&mut l)?;
        e.proyectos = l.split(',').map(str::trim).filter(|x| !x.is_empty()).map(|x| proyecto::de(std::path::Path::new(x))).collect::<Result<_>>()?;
    }
    if let Some(a) = e.autor.clone()
        && !preguntar_si(&tr!("Write «{a}» as the author of each memory?", "¿Apuntar «{a}» como autor de cada memoria?"), true)?
    {
        e.autor = None;
    }
    Ok(e)
}

/// Lo que pasó al unirse con un código de `omoi sync code`.
enum Union {
    /// Unido a un repositorio nuevo para este equipo (su URL, sin credenciales).
    Nueva(String, Option<String>),
    /// Ya sincronizaba con él: se añadió la clave nueva de una rotación.
    Clave(u32),
    /// Ya sincronizaba con él y ya tenía esa clave.
    Ya,
    /// La persona dijo que no.
    Cancelada,
}

/// `omoi sync join <código>` (y cada código de `OMOIKANE_SYNC_CODE`). Con `confirmar`, enseña qué se
/// subirá y, en una terminal, lo pregunta; sin él (la variable ya es la decisión) no escribe nada en
/// la salida, que puede ser el JSON de `omoi --json sync`. El código lleva la clave: no sale en
/// ningún mensaje.
#[allow(clippy::too_many_arguments)]
fn unirse_con(c: &Comun, ruta: &std::path::Path, codigo: &str, otra_url: Option<String>, another_person: bool, space: Option<String>, eleccion: &OpcionesSync, confirmar: bool) -> Result<Union> {
    use omoikane::sync;
    let mut db = abrir(c)?;
    let sync::Codigo { url, clave, id, persona, espacio } = sync::leer_codigo(codigo)?;
    let espacio = espacio_de(space)?.or(espacio);
    if sync::leer_config_de(&sync::rutas_de(ruta, espacio.as_deref()))?.is_some() {
        // Ya sincroniza con ese repositorio. Otra URL (un token nuevo en `OMOIKANE_SYNC_URL`) pasa a ser
        // la suya antes de nada: con la vieja quizá ya no se llega.
        // Si el código resulta ser de otro repositorio, la URL vuelve a ser la de antes.
        let vieja = match &otra_url {
            Some(u) => sync::cambiar_url(ruta, espacio.as_deref(), u)?,
            None => None,
        };
        // La clave nueva de una rotación, junto a las viejas.
        let union = match sync::anadir_clave(&db, ruta, espacio.as_deref(), id, &clave) {
            Ok(true) => Union::Clave(id),
            Ok(false) => Union::Ya,
            Err(e) => {
                if let Some(v) = &vieja {
                    let _ = sync::cambiar_url(ruta, espacio.as_deref(), v);
                }
                return Err(e);
            }
        };
        if let Some(u) = otra_url.as_deref().filter(|_| vieja.is_some()) {
            eprintln!("{}", tr!("the repository URL is now {}", "la URL del repositorio es ahora {}", sync::sin_credenciales(u)));
        }
        return Ok(union);
    }
    let url = otra_url.unwrap_or(url);
    let e = elegir(eleccion)?;
    // Por defecto, otro equipo de la misma persona: su persona, para que los proyectos sin
    // remoto git se junten aunque cada equipo lleve otro nombre de autor (y los que ya había
    // aquí pasan a ella). Con `--another-person`, la de este equipo: nada se junta.
    let local = omoikane::db::persona_de(&db);
    let fija = if another_person { Some(local.clone()) } else { persona };
    let mut config = sync::config_con_persona(&url, &e, fija.as_deref(), &local);
    config.global &= espacio.is_none();
    // El general de otra persona: su clave no es solo de esta (ver `verificado::llaves`).
    config.another_person = another_person && espacio.is_none();
    config.space = espacio;
    if confirmar && !confirmar_subida(&db, &config, &e, eleccion.yes)? {
        return Ok(Union::Cancelada);
    }
    sync::unirse(&mut db, ruta, &config, id, &clave)?;
    sync::adoptar_persona(&mut db, ruta, &config.person)?;
    Ok(Union::Nueva(sync::sin_credenciales(&url), config.space))
}

/// `OMOIKANE_SYNC_CODE` (y `OMOIKANE_SYNC_URL`): se une, sin preguntar, a cada repositorio de esos
/// códigos con el que este equipo aún no sincroniza (o le añade la clave nueva de una rotación). Lo
/// cuenta por la salida de errores, sin el código. Sin la variable no hace nada.
fn unirse_por_entorno(c: &Comun, ruta: &std::path::Path) -> Result<()> {
    let codigos = omoikane::entorno::codigos_sync();
    let url = omoikane::entorno::url_sync();
    anyhow::ensure!(
        url.is_none() || codigos.len() <= 1,
        tr!("OMOIKANE_SYNC_URL goes with a single code in OMOIKANE_SYNC_CODE", "OMOIKANE_SYNC_URL va con un solo código en OMOIKANE_SYNC_CODE")
    );
    // Lo de por defecto, sin preguntar: la variable ya es la decisión.
    let opciones = OpcionesSync { yes: true, share_preferences: false, no_global: false, project: Vec::new(), no_author: false, author: None };
    let varios = codigos.len() > 1;
    for (i, k) in codigos.iter().enumerate() {
        let union = unirse_con(c, ruta, k, url.clone(), false, None, &opciones, false).map_err(|e| {
            // El error de un código no lleva el código (ni su clave): solo cuál de ellos era.
            if varios { e.context(tr!("OMOIKANE_SYNC_CODE (code {})", "OMOIKANE_SYNC_CODE (código {})", i + 1)) } else { e.context("OMOIKANE_SYNC_CODE") }
        })?;
        match union {
            Union::Nueva(u, _) => eprintln!("{}", tr!("joined {u} (OMOIKANE_SYNC_CODE)", "unido a {u} (OMOIKANE_SYNC_CODE)")),
            Union::Clave(id) => eprintln!("{}", tr!("new key k{id} added (OMOIKANE_SYNC_CODE)", "añadida la clave nueva k{id} (OMOIKANE_SYNC_CODE)")),
            Union::Ya | Union::Cancelada => {}
        }
    }
    Ok(())
}

fn sync(c: &Comun, accion: Option<AccionSync>, background: bool, pull: bool) -> Result<()> {
    use omoikane::sync;
    let ruta = ruta_db(c);
    // Son las de Omoikane por dentro: con una acción, una de las dos partes se ignoraría sin decirlo.
    if (pull || background) && accion.is_some() {
        bail!(tr!("`--pull` and `--background` don't go with an action: use them with `omoi sync` alone", "`--pull` y `--background` no van con una acción: úsalos solo con `omoi sync`"));
    }
    // Una CI o un servidor con `OMOIKANE_SYNC_CODE`: se une solo antes de sincronizar.
    if pull || accion.is_none() && !background {
        unirse_por_entorno(c, &ruta)?;
    }
    if pull {
        return sync::traer(&ruta, || abrir(c));
    }
    if background {
        // Sin el modelo: lo que llegue sin vector lo completa la siguiente orden que lo abra.
        return sync::en_segundo_plano(&ruta, || Ok((abrir(c)?, None)));
    }
    let primera = |aviso: &str| -> Result<()> {
        let (mut db, modelo) = abrir_con_modelo(c)?;
        let actual = proyecto_de(c).ok();
        let r = sync::ahora_mismo(&mut db, &ruta, actual.as_deref(), modelo.as_ref())?;
        if c.json {
            return json(&r);
        }
        println!("{aviso}");
        println!("{}", tr!("  {} memories shared; {} received", "  {} memorias compartidas; {} recibidas", r.exportadas, r.nuevas));
        Ok(())
    };
    match accion {
        None => {
            let (mut db, modelo) = abrir_con_modelo(c)?;
            let actual = proyecto_de(c).ok();
            let r = sync::ahora_mismo(&mut db, &ruta, actual.as_deref(), modelo.as_ref())?;
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
        Some(AccionSync::Init { url, create, space, eleccion }) => {
            let mut db = abrir(c)?;
            let espacio = espacio_de(space)?;
            if sync::leer_config_de(&sync::rutas_de(&ruta, espacio.as_deref()))?.is_some() {
                bail!(tr!("sync is already set up here (`omoi sync status`; `omoi sync off` to start over)", "la sincronización ya está configurada aquí (`omoi sync status`; `omoi sync off` para empezar de nuevo)"));
            }
            let url = match (url, create) {
                (Some(u), _) => u,
                (None, Some(nombre)) => crear_repositorio(&nombre)?,
                (None, None) => unreachable!("clap exige url o --create"),
            };
            let e = elegir(&eleccion)?;
            // La persona: si este equipo ya sincroniza otro repositorio, la que ya tiene; si es el
            // primero, la del autor (o el nombre de git), y si no hay, la local.
            let local = omoikane::db::persona_de(&db);
            let ya_sincroniza = !sync::repos(&ruta).is_empty();
            let mut config = sync::config_con_persona(&url, &e, ya_sincroniza.then_some(local.as_str()), &local);
            // Un repositorio de un cliente no lleva las globales (son de la persona, no del cliente).
            config.global &= espacio.is_none();
            config.space = espacio;
            if !confirmar_subida(&db, &config, &e, eleccion.yes)? {
                return Ok(());
            }
            sync::iniciar(&mut db, &ruta, &config)?;
            sync::adoptar_persona(&mut db, &ruta, &config.person)?;
            drop(db);
            primera(&tr!(
                "Sync set up. To add another computer, run `omoi sync code` here and `omoi sync join <code>` there.",
                "Sincronización configurada. Para añadir otro equipo: `omoi sync code` aquí y `omoi sync join <código>` allí."
            ))
        }
        Some(AccionSync::Join { codigo, url: otra_url, another_person, space, eleccion }) => {
            let codigo = match codigo {
                Some(k) if k == "-" => {
                    let mut l = String::new();
                    std::io::stdin().read_line(&mut l)?;
                    l
                }
                Some(k) => k,
                // Sin código: los de `OMOIKANE_SYNC_CODE` (una CI, un servidor), sin preguntar nada.
                None => {
                    anyhow::ensure!(
                        !omoikane::entorno::codigos_sync().is_empty(),
                        tr!("give the code from `omoi sync code` (or set OMOIKANE_SYNC_CODE)", "da el código de `omoi sync code` (o pon OMOIKANE_SYNC_CODE)")
                    );
                    anyhow::ensure!(
                        otra_url.is_none() && space.is_none() && !another_person,
                        tr!("with OMOIKANE_SYNC_CODE, use OMOIKANE_SYNC_URL for another URL", "con OMOIKANE_SYNC_CODE, otra URL va en OMOIKANE_SYNC_URL")
                    );
                    unirse_por_entorno(c, &ruta)?;
                    return primera(&tr!("Joined from OMOIKANE_SYNC_CODE.", "Unido desde OMOIKANE_SYNC_CODE."));
                }
            };
            match unirse_con(c, &ruta, &codigo, otra_url, another_person, space, &eleccion, true)? {
                Union::Ya => {
                    println!("{}", tr!("this computer already has that key", "este equipo ya tiene esa clave"));
                    Ok(())
                }
                Union::Cancelada => Ok(()),
                Union::Clave(id) => primera(&tr!(
                    "New key k{id} added (the old ones stay, to read what others haven't re-encrypted yet).",
                    "Añadida la clave nueva k{id} (las viejas se quedan, para leer lo que otros aún no han vuelto a cifrar)."
                )),
                Union::Nueva(url, espacio) => {
                    primera(&tr!("Joined {}.", "Unido a {}.", url))?;
                    // Lo que acaba de llegar del espacio, y que el agente lo tendrá en su próxima sesión.
                    if !c.json
                        && let Some(espacio) = &espacio
                        && let Some(t) = omoikane::llegada::al_unirse(&abrir(c)?, espacio)?
                    {
                        println!("{t}");
                    }
                    Ok(())
                }
            }
        }
        Some(AccionSync::Code { space }) => {
            println!("{}", sync::codigo(&ruta, espacio_de(space)?.as_deref())?);
            eprintln!("{}", tr!("It carries the key to your memories: share it only through a private channel.", "Lleva la clave de tus memorias: pásalo solo por un canal privado."));
            Ok(())
        }
        Some(AccionSync::Status) => {
            let config = sync::leer_config(&ruta)?.map(|mut cf| {
                cf.url = sync::sin_credenciales(&cf.url);
                cf
            });
            let general = sync::rutas(&ruta);
            let espacios = sync::espacios(&ruta);
            let pausados = if ruta.exists() { proyecto::en_pausa(&db::abrir(&ruta)?)? } else { Vec::new() };
            if c.json {
                let claves = |r: &sync::Rutas| {
                    let (ids, vigente) = sync::claves_de(r);
                    serde_json::json!({ "ids": ids, "current": vigente, "changed": sync::clave_cambiada(r) })
                };
                let de_espacios: Vec<serde_json::Value> = espacios
                    .iter()
                    .map(|e| {
                        let r = sync::rutas_de(&ruta, Some(e));
                        let cf = sync::leer_config_de(&r).ok().flatten().map(|mut cf| {
                            cf.url = sync::sin_credenciales(&cf.url);
                            cf
                        });
                        serde_json::json!({ "space": e, "config": cf, "state": sync::leer_estado_de(&r), "keys": claves(&r) })
                    })
                    .collect();
                return json(&serde_json::json!({
                    "config": config,
                    "state": sync::leer_estado(&ruta),
                    "pending": sync::pendiente(&ruta),
                    "paused": pausados.iter().map(|x| {
                        let mut v = serde_json::to_value(x).unwrap_or_default();
                        v["text"] = sync::texto_pausa(&ruta, x).into();
                        v
                    }).collect::<Vec<_>>(),
                    "keys": config.as_ref().map(|_| claves(&general)),
                    "spaces": de_espacios,
                }));
            }
            println!("{}", estado_sync(&ruta, None));
            for x in &pausados {
                println!("{}{}", tr!("paused     ", "en pausa   "), sync::texto_pausa(&ruta, x));
            }
            for e in &espacios {
                let r = sync::rutas_de(&ruta, Some(e));
                println!("{}{}", tr!("space {e}: ", "espacio {e}: "), estado_sync(&ruta, Some(e)));
                println!("{}{}", tr!("  keys     ", "  claves   "), claves_sync(&r));
            }
            if let Some(cf) = config {
                println!("{}{}", tr!("keys       ", "claves     "), claves_sync(&general));
                let tipos: Vec<&str> = cf.kinds.iter().map(|k| k.as_str()).collect();
                println!("{}{}", tr!("shares     ", "comparte   "), tipos.join(", "));
                println!("{}{}", tr!("global     ", "globales   "), if cf.global { tr!("yes", "sí") } else { "no".into() });
                match &cf.projects {
                    None => println!("{}", tr!("projects   all", "proyectos  todos")),
                    Some(p) => println!("{}{}", tr!("projects   ", "proyectos  "), p.join(", ")),
                }
                println!("{}{}", tr!("author     ", "autor      "), cf.author.as_deref().unwrap_or("-"));
                if let Some(err) = sync::leer_estado(&ruta).ultimo_error {
                    println!("{}{err}", tr!("last error ", "último error "));
                }
            }
            Ok(())
        }
        Some(AccionSync::Restore { id }) => restaurar_cli(c, id),
        Some(AccionSync::Rotate { space, yes }) => {
            let espacio = espacio_de(space)?;
            if sync::leer_config_de(&sync::rutas_de(&ruta, espacio.as_deref()))?.is_none() {
                bail!(tr!("sync is not set up", "la sincronización no está configurada"));
            }
            if !yes {
                anyhow::ensure!(omoikane::entorno::puede_preguntar(), tr!("add --yes to change the key", "añade --yes para cambiar la clave"));
                if !preguntar_si(
                    &tr!(
                        "Change the key? Computers that should keep reading will need the new code (`omoi sync join <code>`).",
                        "¿Cambiar la clave? Los equipos que deban seguir leyendo necesitarán el código nuevo (`omoi sync join <código>`)."
                    ),
                    false,
                )? {
                    return Ok(());
                }
            }
            let (mut db, modelo) = abrir_con_modelo(c)?;
            let actual = proyecto_de(c).ok();
            let (id, r) = sync::rotar(&mut db, &ruta, espacio.as_deref(), actual.as_deref(), modelo.as_ref())?;
            let codigo = sync::codigo(&ruta, espacio.as_deref())?;
            if c.json {
                return json(&serde_json::json!({ "key": id, "exported": r.exportadas, "code": codigo }));
            }
            println!(
                "{}",
                tr!(
                    "New key k{id} in use: {} memories re-encrypted with it. On each computer that should keep reading, run `omoi sync join <code>` with this code:",
                    "Clave nueva k{id} en uso: {} memorias vueltas a cifrar con ella. En cada equipo que deba seguir leyendo, `omoi sync join <código>` con este código:",
                    r.exportadas
                )
            );
            println!("{codigo}");
            eprintln!("{}", tr!("It carries the key to your memories: share it only through a private channel.", "Lleva la clave de tus memorias: pásalo solo por un canal privado."));
            Ok(())
        }
        Some(AccionSync::Off { yes, space }) => {
            let db = abrir(c)?;
            let espacio = espacio_de(space)?;
            if sync::leer_config_de(&sync::rutas_de(&ruta, espacio.as_deref()))?.is_none() {
                println!("{}", tr!("sync is not set up", "la sincronización no está configurada"));
                return Ok(());
            }
            if !yes {
                anyhow::ensure!(omoikane::entorno::puede_preguntar(), tr!("add --yes to stop syncing", "añade --yes para dejar de sincronizar"));
                if !preguntar_si(&tr!("Stop syncing on this computer? Its memories stay; the key is deleted here.", "¿Dejar de sincronizar en este equipo? Sus memorias se quedan; la clave se borra de aquí."), false)? {
                    return Ok(());
                }
            }
            sync::desactivar(&db, &ruta, espacio.as_deref())?;
            println!("{}", tr!("sync is off on this computer", "sincronización apagada en este equipo"));
            Ok(())
        }
        Some(AccionSync::ForgetSealKeys { yes }) => {
            let db = abrir(c)?;
            if !yes {
                anyhow::ensure!(omoikane::entorno::puede_preguntar(), tr!("add --yes to forget them", "añade --yes para olvidarlas"));
                if !preguntar_si(
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

/// Con `OMOIKANE_LANG=es`, la ayuda en español. Los textos en inglés son los comentarios de clap.
fn en_espanol(c: Command) -> Command {
    if !idioma::es() {
        return c;
    }
    let c = c
        .about("Memoria local para agentes de IA: guarda lo que aprenden y se lo devuelve cuando hace falta.")
        .mut_arg("db", |a| a.help("Base de datos (por defecto OMOIKANE_DB, u omoikane.db en OMOIKANE_HOME, que por defecto es ~/.omoikane).").value_name("FICHERO"))
        .mut_arg("dir", |a| a.help("Carpeta cuyo proyecto se usa (por defecto, la actual).").value_name("CARPETA"))
        .mut_arg("json", |a| a.help("Salida en JSON, para otros programas."));
    // (subcomando, descripción, [(argumento, ayuda)])
    type Textos = [(&'static str, &'static str, &'static [(&'static str, &'static str)])];
    let textos: &Textos = &[
        ("remember", "Guarda una memoria. El contenido va como argumento o por la entrada estándar.", &[
            ("kind", "fact, decision, gotcha, preference o procedure."),
            ("title", "Una línea que diga de qué va."),
            ("content", "Qué, por qué y dónde. Sin él (o con «-»), se lee de la entrada estándar."),
            ("paths", "Fichero o carpeta del proyecto al que se refiere (se puede repetir)."),
            ("tags", "Etiqueta libre (se puede repetir). `review:AAAA-MM-DD` la marca para revisar ese día."),
            ("pinned", "Se inyecta siempre en su proyecto."),
            ("global", "Vale para todos los proyectos, no solo para este."),
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
            ("all", "En todos los proyectos."),
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
        ("restore", "Devuelve una memoria de la papelera, en este equipo o en cualquiera sincronizado (sin id, enseña la papelera).", &[("id", "Id de la memoria (basta con el principio).")]),
        ("trash", "Lo que hay en la papelera: memorias olvidadas aquí o en otro equipo, 30 días (OMOIKANE_TRASH_DAYS).", &[
            ("empty", "Vaciarla ahora."),
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
            ("all", "De todos los proyectos."),
        ]),
        ("diary", "El diario de sesiones: una línea por sesión (cuándo, en qué proyecto, ficheros tocados, memorias recibidas y guardadas), de la más reciente a la más vieja. Solo de este equipo.", &[
            ("limit", "Cuántas."),
            ("since", "Sesiones que acabaron ese día o después (2026-11-03, o 7d para los últimos 7 días)."),
            ("until", "Sesiones que empezaron ese día o antes."),
            ("file", "Solo las sesiones que tocaron un fichero cuya ruta contiene esto."),
            ("all", "De todos los proyectos (de su espacio, si el proyecto está en uno)."),
            ("project", "De este proyecto: su carpeta, o su nombre o parte de él."),
            ("space", "De este espacio."),
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
        ("projects", "Proyectos con memorias.", &[]),
        ("trades", "Oficios: el stack de un proyecto (android, web, wordpress…), detectado en sus ficheros. Las memorias de oficio llegan a todos los proyectos de su oficio. Sin acción, los de este proyecto.", &[]),
        ("export", "Una copia fiel: todas las memorias con su historial, las marcas de olvido, los veredictos y la papelera, una línea JSON cada una (`omoi import` la restaura exacta).", &[
            ("all", "De todos los proyectos (por defecto, este y las globales)."),
            ("encrypt", "Cifra cada línea con la clave de la sincronización (AES-SIV), para guardar la copia en cualquier sitio."),
            ("obsidian", "En su lugar: una nota Markdown por memoria en <vault>/omoikane/, para la vista de grafo de Obsidian (un espejo: se rehace en cada exportación). Por defecto, los proyectos personales y las globales."),
            ("okf", "En su lugar: un bundle OKF v0.2 (Open Knowledge Format) en esta carpeta, un concepto por memoria (un espejo: se rehace en cada exportación). Por defecto, los proyectos personales y las globales."),
            ("space", "Con --obsidian o --okf: solo este espacio (un cliente), sin las globales. Cada espacio, en su propia carpeta."),
            ("watch", "Con --obsidian: mantener el vault al día, exportando otra vez cuando cambie la base."),
            ("interval", "Con --watch: segundos entre comprobaciones."),
        ]),
        ("backup", "Copia la base ahora en <su carpeta>/backups/omoikane-<fecha>.db (también se hace sola una vez al día; se conservan OMOIKANE_BACKUPS copias, 7 por defecto).", &[("list", "Solo enseñar las copias que hay.")]),
        ("tui", "Ver, buscar, fijar y olvidar memorias en la terminal.", &[("all", "Empezar con todos los proyectos (por defecto, este y las globales).")]),
        ("import", "Importa un fichero de `omoi export` (exacto: mismos ids, fechas, historial y veredictos; gana el más nuevo, así que se puede repetir), una exportación de engram o un bundle OKF (--okf).", &[
            ("fichero", "El fichero exportado."),
            ("okf", "En su lugar: un bundle OKF v0.2 (una carpeta de conceptos en Markdown, como el knowledge/ de un repositorio). Cada concepto pone al día la misma memoria si se importa otra vez. Al proyecto de esta carpeta o, con --space, a ese espacio."),
            ("space", "Con --okf: a este espacio (un cliente), sin proyecto."),
            ("force", "Importar también lo que parece una memoria ya guardada."),
            ("dry_run", "Solo decir qué se importaría."),
        ]),
        ("model", "El modelo de embeddings que permite buscar por significado.", &[]),
        ("reindex", "Rehace los índices de búsqueda (palabras y vectores) a partir de las memorias.", &[]),
        ("mcp", "Sirve la memoria a un agente de IA por MCP (stdio). Lo arrancan los propios agentes.", &[]),
        ("serve", "Sirve las mismas herramientas por HTTP en 127.0.0.1 (para scripts y apps que no hablan MCP) y una vista web de todas las memorias en http://127.0.0.1:7438/.", &[
            ("port", "El puerto."),
            ("open", "Abrir la vista web en el navegador (si ya hay un servidor en marcha, solo la abre)."),
        ]),
        ("setup", "Prepara un agente para usar Omoikane: claude, codex, gemini, cursor, opencode, qwen, kiro, copilot, windsurf, antigravity, kimi, pi, amp o kilo.", &[
            ("agente", "El agente: claude, codex, gemini, cursor, opencode, qwen, kiro, copilot, windsurf, antigravity, kimi, pi, amp o kilo."),
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
        ]),
        ("doctor", "Comprueba SQLite, FTS5 y sqlite-vec, y dice dónde está la base.", &[]),
        ("sync", "Comparte las memorias entre tus equipos (o con un equipo de personas) por un repositorio git privado tuyo. Sin acción, sincroniza ahora.", &[]),
    ];
    let c = textos.iter().fold(c, |c, (orden, about, args)| {
        c.mut_subcommand(*orden, |s| args.iter().fold(s.about(*about), |s, (arg, ayuda)| s.mut_arg(*arg, |a| a.help(*ayuda))))
    });
    let c = titulos_es(c)
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
                    .mut_arg("ram", |a| a.help("La RAM que puede usar al indexar, en MB (0: volver a la de por defecto). Con más, indexa más rápido por lotes; OMOIKANE_SEMANTIC_RAM manda por encima."))
            })
    });
    let opciones_sync = |s: Command| {
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
    let proyecto_es = |a: Arg| a.help("El proyecto: su carpeta, o su nombre o parte de él (por defecto, el de esta carpeta).").value_name("PROYECTO");
    let c = c.mut_subcommand("trades", |m| {
        m.subcommand_help_heading("Acciones")
            .mut_subcommand("list", |s| s.about("Los oficios de este proyecto y de dónde salen (por defecto); con --all, los de todos.").mut_arg("all", |a| a.help("Todos los proyectos, y cuántas memorias de oficio tiene cada oficio.")))
            .mut_subcommand("add", |s| s.about("El proyecto tiene también estos oficios, aunque no se detecten.").mut_arg("oficios", |a| a.value_name("OFICIO").help("Oficios: android, web, wordpress, seo-geo…")).mut_arg("project", proyecto_es))
            .mut_subcommand("remove", |s| s.about("El proyecto no tiene estos oficios, aunque se detecten.").mut_arg("oficios", |a| a.value_name("OFICIO").help("Oficios: android, web, wordpress, seo-geo…")).mut_arg("project", proyecto_es))
            .mut_subcommand("set", |s| s.about("Exactamente estos oficios (sin ninguno, ninguno), y deja de detectarlos.").mut_arg("oficios", |a| a.value_name("OFICIO").help("Oficios: android, web, wordpress, seo-geo…")).mut_arg("project", proyecto_es))
            .mut_subcommand("auto", |s| s.about("Olvida lo puesto a mano y los vuelve a detectar en sus ficheros.").mut_arg("project", proyecto_es))
    });
    let c = c.mut_subcommand("sync", |m| {
        m.subcommand_help_heading("Acciones")
            .mut_subcommand("init", |s| {
                opciones_sync(s.about("Configura un repositorio compartido nuevo (vacío o aún sin memorias): cualquier URL de git en la que puedas subir."))
                    .mut_arg("url", |a| a.help("El repositorio: git@github.com:tú/memoria.git, https://…, una carpeta…"))
                    .mut_arg("create", |a| {
                        a.help("Crearlo antes como repositorio privado con este nombre, con la línea de órdenes de GitHub (`gh`) o de GitLab (`glab`) si hay una instalada y con sesión iniciada.")
                            .value_name("NOMBRE")
                    })
                    .mut_arg("space", |a| a.help("Un repositorio (con su propia clave) solo para un espacio, además del general.").value_name("NOMBRE"))
            })
            .mut_subcommand("join", |s| {
                opciones_sync(s.about("Únete a un repositorio con el que ya sincroniza otro equipo, con el código de `omoi sync code`."))
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
    let nombres: Vec<String> = c.get_subcommands().map(|s| s.get_name().to_string()).collect();
    let c = nombres.iter().fold(c, |c, n| c.mut_subcommand(n, titulos_es));
    ["model", "sync", "docs", "trades"].iter().fold(c, |c, sub| {
        c.mut_subcommand(*sub, |m| {
            let acciones: Vec<String> = m.get_subcommands().map(|s| s.get_name().to_string()).collect();
            acciones.iter().fold(m.disable_help_subcommand(true), |m, n| m.mut_subcommand(n, titulos_es))
        })
    })
}

/// Los títulos fijos de la ayuda de clap («Usage», «Commands», «Options»…) en español.
fn titulos_es(c: Command) -> Command {
    c.help_template("{about-with-newline}\nUso: {usage}\n\n{all-args}{after-help}")
        .subcommand_help_heading("Comandos")
        .mut_args(|a| {
            let titulo = if a.is_positional() { "Argumentos" } else { "Opciones" };
            a.help_heading(titulo)
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

/// `omoi setup codex|gemini|cursor|opencode`: cambia los ficheros de configuración del agente.
fn setup_otro(agente: &str, si: bool, simulacro: bool) -> Result<()> {
    let cambios = omoikane::agentes::plan(agente)?;
    if cambios.is_empty() {
        println!("{}", tr!("{agente} already has Omoikane: nothing to do.", "{agente} ya tiene Omoikane: nada que hacer."));
        return Ok(());
    }
    println!("{}", tr!("To set up {agente}, omoi will change:", "Para preparar {agente}, omoi va a cambiar:"));
    for c in &cambios {
        println!("  - {}   ({})", c.fichero.display(), c.que);
    }
    println!("{}", tr!("  (a copy of each file is kept as <file>.antes-de-omoikane)", "  (se guarda una copia de cada fichero como <fichero>.antes-de-omoikane)"));
    if simulacro {
        return Ok(());
    }
    if !si && !preguntar()? {
        println!("{}", tr!("nothing changed", "no se ha cambiado nada"));
        return Ok(());
    }
    omoikane::agentes::aplicar(&cambios)?;
    println!("{}", tr!("Done. Start a new {agente} session.", "Hecho. Abre una sesión nueva de {agente}."));
    Ok(())
}

/// «¿Sigo?» en una terminal; fuera de ella hace falta --yes.
fn preguntar() -> Result<bool> {
    if !omoikane::entorno::puede_preguntar() {
        bail!(tr!("add --yes to do it without asking", "añade --yes para hacerlo sin preguntar"));
    }
    eprint!("{}", tr!("Go ahead? [y/N] ", "¿Sigo? [s/N] "));
    std::io::stderr().flush()?;
    let mut linea = String::new();
    std::io::stdin().read_line(&mut linea)?;
    Ok(matches!(linea.trim().to_lowercase().as_str(), "y" | "yes" | "s" | "si" | "sí"))
}

/// `omoi restore [id]` (y `omoi sync restore`, que es lo mismo): sin id, la papelera.
fn restaurar_cli(c: &Comun, id: Option<String>) -> Result<()> {
    let Some(id) = id else {
        return listar_papelera(c, &abrir(c)?);
    };
    let (mut db, modelo) = abrir_con_modelo(c)?;
    let antes = omoikane::revisar::antes(&db)?;
    let m = memoria::restaurar(&mut db, &id, modelo.as_ref())?;
    let _ = omoikane::revisar::al_dia_de(&db, &antes, std::slice::from_ref(&m.id));
    omoikane::sync::programar(&c.db.clone().unwrap_or_else(db::ruta_por_defecto));
    if c.json {
        return json(&m);
    }
    println!("{}", tr!("restored {}: {}", "recuperada {}: {}", corto(&m.id), m.title));
    Ok(())
}

fn listar_papelera(c: &Comun, db: &Connection) -> Result<()> {
    let v = memoria::papelera(db)?;
    if c.json {
        return json(&v);
    }
    if v.is_empty() {
        println!("{}", tr!("the trash is empty", "la papelera está vacía"));
    }
    for o in v {
        println!("{}  {}  ({}, {})", corto(&o.id), o.title.chars().take(90).collect::<String>(), o.by, o.deleted_at.get(..10).unwrap_or(&o.deleted_at));
    }
    Ok(())
}

/// En una terminal, pregunta; en cualquier otro sitio (un agente, un script) se niega sin `--yes`,
/// para que nadie borre por no haber leído la ayuda.
fn confirmar_olvido(m: &Memoria, purge: bool) -> Result<bool> {
    if !omoikane::entorno::puede_preguntar() {
        bail!(tr!("add --yes to forget {} without asking", "añade --yes para olvidar {} sin preguntar", corto(&m.id)));
    }
    if purge {
        eprint!("{}", tr!("Delete «{}» and its history for good? It can't be undone [y/N] ", "¿Borrar «{}» y su historial del todo? No se puede deshacer [s/N] ", m.title));
    } else {
        eprint!("{}", tr!("Forget «{}»? It goes to the trash with its history; `omoi restore` brings it back [y/N] ", "¿Olvidar «{}»? Va a la papelera con su historial; `omoi restore` la devuelve [s/N] ", m.title));
    }
    std::io::stderr().flush()?;
    let mut linea = String::new();
    std::io::stdin().read_line(&mut linea)?;
    Ok(matches!(linea.trim().to_lowercase().as_str(), "y" | "yes" | "s" | "si" | "sí"))
}
