//! `omoi`: memoria local para agentes de IA. Un binario, un fichero SQLite.

use anyhow::{Context, Result, bail};
use clap::{Arg, ArgAction, Args, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use omoikane::buscar::{self, Opciones};
use omoikane::memoria::{self, Ambito, Cambios, Memoria, Nueva, Tipo, Version};
use omoikane::embeber::Modelo;
use omoikane::{db, idioma, modelos, proyecto, recall, tr, vectores};
use rusqlite::Connection;
use std::io::{Read, Write};
use std::path::PathBuf;

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
    /// Database file (default: OMOIKANE_DB or ~/.omoikane/omoikane.db).
    #[arg(long, global = true, value_name = "FILE")]
    db: Option<PathBuf>,
    /// Folder whose project to use (default: the current one).
    #[arg(long, global = true, value_name = "DIR")]
    dir: Option<PathBuf>,
    /// JSON output, for other programs.
    #[arg(long, global = true)]
    json: bool,
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
        /// Free tag (repeatable).
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
        /// Who saves it (for example "claude-code:<session>").
        #[arg(long, default_value = "cli")]
        source: String,
        /// Save it even if a very similar memory already exists.
        #[arg(short, long)]
        force: bool,
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
        id: String,
    },
    /// Change a memory. The previous version is kept in its history.
    Update {
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
    },
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
        /// With --dismiss: which warning (outdated, stale_status, broken_path, unused, too_long, secret).
        #[arg(long, requires = "dismiss")]
        kind: Option<String>,
        /// With --dismiss on an outdated warning: the id of the newer memory.
        #[arg(long, requires = "dismiss")]
        other: Option<String>,
    },
    /// A faithful copy: every memory with its history, forget marks, verdicts and the trash, one JSON line each (`omoi import` restores it exactly).
    Export {
        /// From every project (default: this one and the global ones).
        #[arg(long)]
        all: bool,
        /// Encrypt every line with the sync key (AES-SIV), to keep the copy anywhere.
        #[arg(long)]
        encrypt: bool,
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
    /// Import an `omoi export` file (exactly: same ids, dates, history and verdicts; newest wins, so it is safe to repeat) or an engram export.
    Import {
        /// The export file.
        fichero: PathBuf,
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
    },
    /// Share memories between your computers (or with a team) through a private git repository of
    /// yours. Without an action, sync now.
    Sync {
        #[command(subcommand)]
        accion: Option<AccionSync>,
        /// Run in the background after a change (Omoikane launches it itself).
        #[arg(long, hide = true)]
        background: bool,
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
        /// When a session ends or compacts: set aside what may be worth saving from the conversation
        /// (`transcript_path`), for the agent of the next session. Prints nothing.
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
        /// the shell history).
        codigo: String,
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
enum AccionModelo {
    /// Which model is installed and in use (the default).
    Status,
    /// Download the model (about 150 MB, once) and compile it.
    Install {
        /// Take it from a folder with the int8 model instead of downloading it.
        #[arg(long, value_name = "DIR")]
        from: Option<PathBuf>,
    },
    /// Delete installed models that are not in use.
    Clean,
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
    let c = &cli.comun;
    if !matches!(cli.orden, Orden::Upgrade { .. }) {
        omoikane::actualizar::quizas_en_segundo_plano();
    }
    let escribe = matches!(cli.orden, Orden::Remember { .. } | Orden::Update { .. } | Orden::Forget { .. } | Orden::Restore { .. } | Orden::Import { .. } | Orden::Tui { .. });
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
    r
}

fn ruta_db(c: &Comun) -> PathBuf {
    c.db.clone().unwrap_or_else(db::ruta_por_defecto)
}

fn ejecutar(c: &Comun, orden: Orden) -> Result<()> {
    match orden {
        Orden::Doctor => doctor(c),
        Orden::Model { accion } => modelo(c, accion.unwrap_or(AccionModelo::Status)),
        Orden::Mcp => omoikane::mcp::servir(c.db.clone().unwrap_or_else(db::ruta_por_defecto), c.dir.clone()),
        Orden::Serve { port, open } => omoikane::http::servir(c.db.clone().unwrap_or_else(db::ruta_por_defecto), c.dir.clone(), port, open),
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
            std::process::exit(if es_error { 1 } else { 0 })
        }
        Orden::Setup { agente, yes, dry_run, no_auto_update } => setup(&agente, yes, dry_run, no_auto_update),
        Orden::Upgrade { check, on, off, background, after } => upgrade(check, on, off, background, after),
        Orden::Sync { accion, background } => sync(c, accion, background),
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
        Orden::Remember { kind, title, content, paths, tags, pinned, global, space, source, force } => {
            let content = texto_o_entrada(content)?;
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
            if !force
                && let Some(modelo) = &modelo
                && let Some((existente, sim)) = vectores::duplicada(&db, modelo, &busca_en, &title, &content)?
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
            let m = memoria::guardar(
                &mut db,
                Nueva { project, space_id, kind: Some(kind), title, content, paths, tags, pinned, source, ..Default::default() },
                modelo.as_ref(),
            )?;
            let _ = omoikane::revisar::al_dia_de(&db, &antes, std::slice::from_ref(&m.id));
            avisar_redactados(&m);
            let rel = vectores::relacionadas(&db, &busca_en, &m.title, &m.id, 3)?;
            if c.json {
                #[derive(serde::Serialize)]
                struct Guardada<'a> {
                    #[serde(flatten)]
                    memoria: &'a Memoria,
                    #[serde(skip_serializing_if = "<[Memoria]>::is_empty")]
                    related: &'a [Memoria],
                }
                return json(&Guardada { memoria: &m, related: &rel });
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
        Orden::Get { id } => {
            let m = memoria::obtener(&abrir(c)?, &id)?;
            if c.json {
                return json(&m);
            }
            entera(&m);
            Ok(())
        }
        Orden::Update { id, title, content, kind, paths, no_paths, tags, no_tags, pin, unpin, project, global, space, reason, replace, with, append } => {
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
            let antes = omoikane::revisar::antes(&db)?;
            let (m, cambiados) = memoria::actualizar_con_cambios(&mut db, &id, cambios, modelo.as_ref())?;
            let _ = omoikane::revisar::al_dia_de(&db, &antes, std::slice::from_ref(&m.id));
            avisar_redactados(&m);
            if c.json {
                let mut v = serde_json::to_value(&m)?;
                if !cambiados.is_empty() {
                    v["changed"] = serde_json::json!(cambiados);
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
        Orden::Trash { empty, yes } => {
            let db = abrir(c)?;
            if !empty {
                return listar_papelera(c, &db);
            }
            let n = memoria::papelera(&db)?.len();
            if !yes {
                use std::io::IsTerminal;
                anyhow::ensure!(std::io::stdin().is_terminal(), tr!("add --yes to empty the trash without asking", "añade --yes para vaciar la papelera sin preguntar"));
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
            let v = memoria::historial(&db, &m.id)?;
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
        Orden::Review { all, dismiss, kind, other } => {
            let db = abrir(c)?;
            if let Some(id) = dismiss {
                let otra = match other {
                    Some(o) => memoria::resolver_id(&db, &o)?,
                    None => String::new(),
                };
                let kind = kind.unwrap_or_else(|| if otra.is_empty() { omoikane::revisar::LARGA.into() } else { omoikane::revisar::DESFASADA.into() });
                omoikane::revisar::descartar(&db, &id, &kind, &otra)?;
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
            let cs = omoikane::candidatas::todas(&db)?;
            if v.is_empty() && cs.is_empty() && pausados.is_empty() {
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
        Orden::Tui { all } => {
            let (mut db, modelo) = abrir_con_modelo(c)?;
            let proyecto = proyecto_de(c)?;
            let all = all || proyecto::es_general(&proyecto);
            omoikane::tui::ejecutar(&mut db, modelo.as_ref(), proyecto, all)
        }
        Orden::Import { fichero, force, dry_run } => {
            let actual = proyecto_de(c)?;
            if omoikane::copia::es_propio(&fichero)? {
                let ruta = c.db.clone().unwrap_or_else(db::ruta_por_defecto);
                let (mut db, modelo) = abrir_con_modelo(c)?;
                let (cuenta, r) = omoikane::copia::importar(&mut db, &ruta, &fichero, &actual, modelo.as_ref(), dry_run)?;
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
        Orden::Export { all, encrypt } => {
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
    use std::io::IsTerminal;
    if simulacro {
        return Ok(());
    }
    let on = if no {
        false
    } else if let Some(ya) = omoikane::actualizar::preferencia().filter(|_| si || !std::io::stdin().is_terminal()) {
        ya
    } else if si || !std::io::stdin().is_terminal() {
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

/// `omoi recall --session-end`: aparta candidatas de la conversación. Un fallo nunca molesta.
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
    if e.transcript_path.is_empty() || !std::path::Path::new(&e.transcript_path).exists() || !ruta_db.exists() {
        return Ok(());
    }
    let project = match (&c.dir, e.cwd.is_empty()) {
        (Some(d), _) => proyecto::de(d)?,
        (None, false) => proyecto::de(std::path::Path::new(&e.cwd))?,
        (None, true) => proyecto::actual()?,
    };
    let db = db::abrir(&ruta_db)?;
    if let Err(err) = omoikane::candidatas::apartar(&db, &project, &e.session_id, std::path::Path::new(&e.transcript_path)) {
        eprintln!("omoikane: {err:#}");
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
        (None, false) => proyecto::de(std::path::Path::new(&e.cwd))?,
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
        if let Ok(Some(_)) = omoikane::sync::leer_config(&ruta) {
            let _ = omoikane::sync::adoptar(&db, &project);
            omoikane::sync::programar(&ruta);
        }
        let texto = recall::al_empezar(&mut db, &project, &e)?.map(|t| match omoikane::sync::aviso(&ruta) {
            Some(a) => t.replacen("</omoikane>", &format!("{a}\n</omoikane>"), 1),
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
        AccionModelo::Install { from } => {
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
fn ambito_pedido(c: &Comun, db: &Connection, todos: bool, project: Option<&str>, space: Option<&str>) -> Result<Ambito> {
    let base = Ambito::de(db, &proyecto_de(c)?)?;
    let pedido = if let Some(s) = space {
        Ambito::Espacio(Some(proyecto::buscar_espacio(db, s)?))
    } else if let Some(p) = project {
        let id = proyecto::buscar(db, p)?;
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
            let v = proyecto::lista(&db, None, filtro, usize::MAX)?;
            if c.json {
                return json(&v);
            }
            for p in &v {
                let espacio = if p.space.is_empty() { String::new() } else { format!(" [{}]", p.space) };
                let ido = if p.gone { tr!("  (folder gone)", "  (ya no está)") } else { String::new() };
                println!("{:>5}  {}  {}{espacio}  {}{ido}", p.memories, p.last_update.get(..10).unwrap_or(""), p.name, p.path);
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
    println!("{}  {:<10} {}{fijada}", corto(&m.id), m.kind.as_str(), m.title);
    let mut extra: Vec<String> = m.paths.clone();
    extra.extend(m.tags.iter().map(|t| format!("#{t}")));
    if m.project.is_empty() {
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
    let proyecto = if m.project.is_empty() { "global" } else { &m.project };
    println!("{}{proyecto}", tr!("project    ", "proyecto   "));
    println!("{}{}", tr!("source     ", "origen     "), m.source);
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
    use std::io::IsTerminal;
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
        std::io::stdin().is_terminal(),
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
    use std::io::IsTerminal;
    let proyectos = o.project.iter().map(|p| proyecto::de(p)).collect::<Result<Vec<_>>>()?;
    let mut e = omoikane::sync::Eleccion {
        preferencias: o.share_preferences,
        global: !o.no_global,
        proyectos,
        espacios: Vec::new(),
        autor: if o.no_author { None } else { o.author.clone().or_else(omoikane::sync::nombre_git) },
    };
    if o.yes || !std::io::stdin().is_terminal() {
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

fn sync(c: &Comun, accion: Option<AccionSync>, background: bool) -> Result<()> {
    use omoikane::sync;
    use std::io::IsTerminal;
    let ruta = ruta_db(c);
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
            let mut db = abrir(c)?;
            let codigo = if codigo == "-" {
                let mut l = String::new();
                std::io::stdin().read_line(&mut l)?;
                l
            } else {
                codigo
            };
            let sync::Codigo { url, clave, id, persona, espacio } = sync::leer_codigo(&codigo)?;
            let espacio = espacio_de(space)?.or(espacio);
            if sync::leer_config_de(&sync::rutas_de(&ruta, espacio.as_deref()))?.is_some() {
                // Ya sincroniza con ese repositorio: la clave nueva de una rotación, junto a las viejas.
                if !sync::anadir_clave(&db, &ruta, espacio.as_deref(), id, &clave)? {
                    println!("{}", tr!("this computer already has that key", "este equipo ya tiene esa clave"));
                    return Ok(());
                }
                drop(db);
                return primera(&tr!(
                    "New key k{id} added (the old ones stay, to read what others haven't re-encrypted yet).",
                    "Añadida la clave nueva k{id} (las viejas se quedan, para leer lo que otros aún no han vuelto a cifrar)."
                ));
            }
            let url = otra_url.unwrap_or(url);
            let e = elegir(&eleccion)?;
            // Por defecto, otro equipo de la misma persona: su persona, para que los proyectos sin
            // remoto git se junten aunque cada equipo lleve otro nombre de autor (y los que ya había
            // aquí pasan a ella). Con `--another-person`, la de este equipo: nada se junta.
            let local = omoikane::db::persona_de(&db);
            let fija = if another_person { Some(local.clone()) } else { persona };
            let mut config = sync::config_con_persona(&url, &e, fija.as_deref(), &local);
            config.global &= espacio.is_none();
            config.space = espacio;
            if !confirmar_subida(&db, &config, &e, eleccion.yes)? {
                return Ok(());
            }
            sync::unirse(&mut db, &ruta, &config, id, &clave)?;
            sync::adoptar_persona(&mut db, &ruta, &config.person)?;
            drop(db);
            primera(&tr!("Joined {}.", "Unido a {}.", sync::sin_credenciales(&url)))
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
                anyhow::ensure!(std::io::stdin().is_terminal(), tr!("add --yes to change the key", "añade --yes para cambiar la clave"));
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
                anyhow::ensure!(std::io::stdin().is_terminal(), tr!("add --yes to stop syncing", "añade --yes para dejar de sincronizar"));
                if !preguntar_si(&tr!("Stop syncing on this computer? Its memories stay; the key is deleted here.", "¿Dejar de sincronizar en este equipo? Sus memorias se quedan; la clave se borra de aquí."), false)? {
                    return Ok(());
                }
            }
            sync::desactivar(&db, &ruta, espacio.as_deref())?;
            println!("{}", tr!("sync is off on this computer", "sincronización apagada en este equipo"));
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
        .mut_arg("db", |a| a.help("Base de datos (por defecto OMOIKANE_DB o ~/.omoikane/omoikane.db).").value_name("FICHERO"))
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
            ("tags", "Etiqueta libre (se puede repetir)."),
            ("pinned", "Se inyecta siempre en su proyecto."),
            ("global", "Vale para todos los proyectos, no solo para este."),
            ("source", "Quién la guarda (por ejemplo «claude-code:<sesión>»)."),
            ("force", "Guardarla aunque ya exista una muy parecida."),
        ]),
        ("search", "Busca en las memorias de este proyecto y en las globales.", &[
            ("query", "Lo que buscas, en lenguaje normal."),
            ("kind", "Solo de este tipo."),
            ("limit", "Cuántos resultados."),
            ("tags", "Solo las que llevan esta etiqueta (se puede repetir)."),
            ("all", "En todos los proyectos."),
        ]),
        ("get", "Una memoria entera (basta con el principio del id).", &[("id", "Id de la memoria (basta con el principio).")]),
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
            ("reason", "Por qué cambia (se guarda con la versión anterior)."),
            ("replace", "Trozo exacto del contenido que se cambia; tiene que aparecer exactamente una vez. Cada uno con su --with (se puede repetir)."),
            ("with", "Lo que va en lugar del --replace que le toca (se puede repetir, en el mismo orden)."),
            ("append", "Texto que se añade al final del contenido, tras una línea en blanco (un paso más en una receta)."),
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
        ("review", "Lo que conviene revisar: memorias que otra más nueva contradice, estados viejos, rutas rotas, sin uso y demasiado largas.", &[
            ("all", "De todos los proyectos."),
            ("dismiss", "«Está bien así»: deja de avisar de esta memoria (hasta que cambie). Su id."),
            ("kind", "Con --dismiss: qué aviso (outdated, stale_status, broken_path, unused, too_long)."),
            ("other", "Con --dismiss en un aviso outdated: el id de la memoria más nueva."),
        ]),
        ("recent", "Las últimas memorias guardadas o cambiadas.", &[
            ("limit", "Cuántas."),
            ("kind", "Solo de este tipo."),
            ("all", "De todos los proyectos."),
        ]),
        ("projects", "Proyectos con memorias.", &[]),
        ("export", "Una copia fiel: todas las memorias con su historial, las marcas de olvido, los veredictos y la papelera, una línea JSON cada una (`omoi import` la restaura exacta).", &[
            ("all", "De todos los proyectos (por defecto, este y las globales)."),
            ("encrypt", "Cifra cada línea con la clave de la sincronización (AES-SIV), para guardar la copia en cualquier sitio."),
        ]),
        ("backup", "Copia la base ahora en <su carpeta>/backups/omoikane-<fecha>.db (también se hace sola una vez al día; se conservan OMOIKANE_BACKUPS copias, 7 por defecto).", &[("list", "Solo enseñar las copias que hay.")]),
        ("tui", "Ver, buscar, fijar y olvidar memorias en la terminal.", &[("all", "Empezar con todos los proyectos (por defecto, este y las globales).")]),
        ("import", "Importa un fichero de `omoi export` (exacto: mismos ids, fechas, historial y veredictos; gana el más nuevo, así que se puede repetir) o una exportación de engram.", &[
            ("fichero", "El fichero exportado."),
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
                "Al cerrar o compactar una sesión: aparta lo que quizá valga la pena guardar de la conversación (`transcript_path`) para el agente de la sesión siguiente. No escribe nada.",
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
            })
            .mut_subcommand("clean", |s| s.about("Borra los modelos instalados que no se usan."))
    });
    let opciones_sync = |s: Command| {
        s.mut_arg("yes", |a| a.help("No preguntar: usar estas opciones y las de por defecto."))
            .mut_arg("share_preferences", |a| a.help("Compartir también las preferencias (por defecto se quedan en este equipo)."))
            .mut_arg("no_global", |a| a.help("No compartir las memorias globales."))
            .mut_arg("project", |a| a.help("Compartir solo este proyecto (se puede repetir; por defecto, todos).").value_name("CARPETA"))
            .mut_arg("no_author", |a| a.help("No apuntar autor en cada memoria."))
            .mut_arg("author", |a| a.help("El autor que se apunta en cada memoria (por defecto, el user.name de git)."))
    };
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
                    .mut_arg("codigo", |a| a.help("El código que escribe `omoi sync code` («-» lo lee de la entrada estándar, para que no quede en el historial de la terminal).").value_name("CÓDIGO"))
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
    });
    let nombres: Vec<String> = c.get_subcommands().map(|s| s.get_name().to_string()).collect();
    let c = nombres.iter().fold(c, |c, n| c.mut_subcommand(n, titulos_es));
    ["model", "sync"].iter().fold(c, |c, sub| {
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
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
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
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
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
