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
        /// fact, decision, gotcha or preference.
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
        /// In every project.
        #[arg(long)]
        all: bool,
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
        /// Why it changes (kept with the previous version).
        #[arg(short, long)]
        reason: Option<String>,
    },
    /// Delete a memory and its history. It can't be undone.
    Forget {
        /// Memory id (its start is enough).
        id: String,
        /// Don't ask. Without it, omoi asks in a terminal and refuses elsewhere.
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
        /// From every project.
        #[arg(long)]
        all: bool,
    },
    /// Projects that have memories.
    Projects,
    /// Every memory with its history, one JSON object per line.
    Export {
        /// From every project (default: this one and the global ones).
        #[arg(long)]
        all: bool,
    },
    /// Browse, search, pin and forget memories in the terminal.
    Tui {
        /// Start with every project (default: this one and the global ones).
        #[arg(long)]
        all: bool,
    },
    /// Import memories from an engram export (`engram export`) or an `omoi export` file.
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
    },
    /// For Claude Code hooks: reads the hook's JSON on stdin and prints the memories to inject.
    Recall {
        /// At session start (or after compacting): preferences and pinned memories.
        #[arg(long)]
        session_start: bool,
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
enum AccionSync {
    /// Set up a new shared repository (empty, or without memories yet): any git URL you can push to.
    Init {
        /// The repository: git@github.com:you/memory.git, https://…, a folder…
        url: String,
        #[command(flatten)]
        eleccion: OpcionesSync,
    },
    /// Join a repository another computer already syncs with, with the code from `omoi sync code`.
    Join {
        /// The code printed by `omoi sync code`.
        codigo: String,
        /// Reach the same repository by another URL (for example SSH here and HTTPS on the other computer).
        #[arg(long)]
        url: Option<String>,
        #[command(flatten)]
        eleccion: OpcionesSync,
    },
    /// Print the code to join from another computer. It carries the key: share it privately.
    Code,
    /// How sync is set up and how the last one went.
    Status,
    /// Stop syncing on this computer (its memories stay).
    Off {
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
    let cli = Cli::from_arg_matches(&en_espanol(Cli::command()).get_matches()).unwrap_or_else(|e| e.exit());
    let c = &cli.comun;
    let escribe = matches!(cli.orden, Orden::Remember { .. } | Orden::Update { .. } | Orden::Forget { .. } | Orden::Import { .. } | Orden::Tui { .. });
    let r = ejecutar(c, cli.orden);
    if escribe && r.is_ok() {
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
        Orden::Setup { agente, yes, dry_run } => setup(&agente, yes, dry_run),
        Orden::Sync { accion, background } => sync(c, accion, background),
        Orden::Recall { session_start, format } => {
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
        Orden::Remember { kind, title, content, paths, tags, pinned, global, source, force } => {
            let content = texto_o_entrada(content)?;
            // Sin --dir, desde una carpeta que no es un proyecto (el home) la memoria va al repositorio
            // al que apunten sus rutas, o es global.
            let actual = proyecto_de(c)?;
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
            let m = memoria::guardar(
                &mut db,
                Nueva { project, kind: Some(kind), title, content, paths, tags, pinned, source },
                modelo.as_ref(),
            )?;
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
        Orden::Search { query, kind, tags, limit, all } => {
            let o = Opciones { ambito: ambito(c, all)?, kind, tags, limite: limit };
            let (mut db, modelo) = abrir_con_modelo(c)?;
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
        Orden::Update { id, title, content, kind, paths, no_paths, tags, no_tags, pin, unpin, project, global, reason } => {
            let cambios = Cambios {
                title,
                content: content.map(|t| texto_o_entrada(Some(t))).transpose()?,
                kind,
                paths: if no_paths { Some(Vec::new()) } else { (!paths.is_empty()).then_some(paths) },
                tags: if no_tags { Some(Vec::new()) } else { (!tags.is_empty()).then_some(tags) },
                pinned: if pin { Some(true) } else if unpin { Some(false) } else { None },
                project: if global { Some(String::new()) } else { project.as_deref().map(proyecto::de_carpeta).transpose()? },
                reason,
            };
            let (mut db, modelo) = abrir_con_modelo(c)?;
            let m = memoria::actualizar(&mut db, &id, cambios, modelo.as_ref())?;
            if c.json {
                return json(&m);
            }
            println!("{}", tr!("updated {}", "actualizada {}", corto(&m.id)));
            Ok(())
        }
        Orden::Forget { id, yes } => {
            let mut db = abrir(c)?;
            let m = memoria::obtener(&db, &id)?;
            if !yes && !confirmar_olvido(&m)? {
                println!("{}", tr!("nothing deleted", "no se ha borrado nada"));
                return Ok(());
            }
            let m = memoria::olvidar(&mut db, &m.id)?;
            if c.json {
                return json(&m);
            }
            println!("{}", tr!("forgotten {}: {}", "olvidada {}: {}", corto(&m.id), m.title));
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
        Orden::Recent { limit, kind, all } => {
            let v = memoria::recientes(&abrir(c)?, &ambito(c, all)?, kind, limit)?;
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
        Orden::Projects => {
            let v = memoria::proyectos(&abrir(c)?)?;
            if c.json {
                return json(&v);
            }
            for p in &v {
                let globales = tr!("(global)", "(globales)");
                let nombre = if p.project.is_empty() { &globales } else { &p.project };
                println!("{:>5}  {}  {}", p.memories, &p.last_update[..10], nombre);
            }
            Ok(())
        }
        Orden::Tui { all } => {
            let (mut db, modelo) = abrir_con_modelo(c)?;
            let proyecto = proyecto_de(c)?;
            let all = all || proyecto::es_general(&proyecto);
            omoikane::tui::ejecutar(&mut db, modelo.as_ref(), proyecto, all)
        }
        Orden::Import { fichero, force, dry_run } => {
            let actual = proyecto_de(c)?;
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
        Orden::Export { all } => {
            let mut salida = std::io::stdout().lock();
            let ambito = if all { Ambito::Todos } else { Ambito::Proyecto(proyecto_de(c)?) };
            let n = memoria::exportar(&abrir(c)?, &ambito, &mut salida)?;
            salida.flush()?;
            eprintln!("{}", tr!("{n} memories exported", "{n} memorias exportadas"));
            Ok(())
        }
    }
}

fn setup(agente: &str, si: bool, simulacro: bool) -> Result<()> {
    if agente != "claude" {
        return setup_otro(agente, si, simulacro);
    }
    let pasos = omoikane::setup::plan()?;
    if pasos.is_empty() {
        println!("{}", tr!("Claude Code already has Omoikane: nothing to do.", "Claude Code ya tiene Omoikane: nada que hacer."));
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
    println!(
        "{}",
        tr!(
            "Done. Open a new Claude Code session: the memory tools and the automatic recall are there.",
            "Hecho. Abre una sesión nueva de Claude Code: ahí están las herramientas de memoria y el recall automático."
        )
    );
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
        if let Ok(Some(config)) = omoikane::sync::leer_config(&ruta) {
            let _ = omoikane::sync::adoptar(&db, &project, &config.person);
            omoikane::sync::programar(&ruta);
        }
        let texto = recall::al_empezar(&mut db, &project, &e)?.map(|t| match omoikane::sync::aviso(&ruta) {
            Some(a) => t.replacen("</omoikane>", &format!("{a}\n</omoikane>"), 1),
            None => t,
        });
        ("SessionStart", texto)
    } else {
        // El modelo abre en milisegundos (mapeado); sin él, el recall va solo por palabras.
        let modelo = modelos::abrir_actual().ok().flatten();
        ("UserPromptSubmit", recall::al_preguntar(&mut db, &project, &e, modelo.as_ref())?)
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

fn ambito(c: &Comun, todos: bool) -> Result<Ambito> {
    Ok(if todos { Ambito::Todos } else { Ambito::de(&proyecto_de(c)?) })
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

fn json(v: &impl serde::Serialize) -> Result<()> {
    let mut salida = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut salida, v)?;
    writeln!(salida)?;
    Ok(())
}

/// Lo que enseña la CLI del id: 10 caracteres de tiempo y 2 aleatorios. `get` acepta este principio.
fn corto(id: &str) -> &str {
    &id[..id.len().min(12)]
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
        let p: i64 = base.query_row("SELECT count(DISTINCT project) FROM memory", [], |f| f.get(0))?;
        let v = db::VERSION_ESQUEMA;
        let r = ruta.display();
        let memorias = if n == 1 { tr!("1 memory", "1 memoria") } else { tr!("{n} memories", "{n} memorias") };
        let proyectos = if p == 1 { tr!("1 project", "1 proyecto") } else { tr!("{p} projects", "{p} proyectos") };
        println!("{}", tr!("database   {r} ({memorias}, {proyectos}, schema {v})", "base       {r} ({memorias}, {proyectos}, esquema {v})"));
    } else {
        let r = ruta.display();
        println!("{}", tr!("database   {r} (not created yet: it appears with the first memory)", "base       {r} (todavía no existe: se crea al guardar la primera memoria)"));
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
    println!("{}{}", tr!("sync       ", "sincroniz. "), estado_sync(&ruta_db(c)));
    println!("{}", tr!("language   en (OMOIKANE_LANG=es for Spanish)", "idioma     es (OMOIKANE_LANG)"));
    Ok(())
}

/// Una línea con el estado de la sincronización (para `doctor` y `sync status`).
fn estado_sync(ruta: &std::path::Path) -> String {
    use omoikane::sync;
    let Ok(Some(config)) = sync::leer_config(ruta) else {
        return tr!("off (`omoi sync init <git url>` to share memories between computers)", "apagada (`omoi sync init <url de git>` para compartir memorias entre equipos)");
    };
    let e = sync::leer_estado(ruta);
    let hace = |s: &str| {
        let t = s.parse::<u64>().unwrap_or(0);
        let ahora = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let m = ahora.saturating_sub(t) / 60;
        if m < 60 { tr!("{m} min ago", "hace {m} min") } else { tr!("{} h ago", "hace {} h", m / 60) }
    };
    let mut l = config.url.clone();
    match (&e.ultimo_error, &e.ultimo_ok) {
        (Some(err), _) => l.push_str(&tr!(" · FAILING: {}", " · FALLA: {}", err.lines().next().unwrap_or_default())),
        (None, Some(ok)) => l.push_str(&tr!(" · last ok {}", " · última bien {}", hace(ok))),
        (None, None) => l.push_str(&tr!(" · not synced yet", " · aún sin sincronizar")),
    }
    if sync::pendiente(ruta) {
        l.push_str(&tr!(" · changes pending", " · cambios pendientes"));
    }
    l
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

/// Qué compartir: lo de las opciones y, en una terminal sin `--yes`, lo que conteste el usuario.
fn elegir(o: &OpcionesSync) -> Result<omoikane::sync::Eleccion> {
    use std::io::IsTerminal;
    let proyectos = o.project.iter().map(|p| proyecto::de(p)).collect::<Result<Vec<_>>>()?;
    let mut e = omoikane::sync::Eleccion {
        preferencias: o.share_preferences,
        global: !o.no_global,
        proyectos,
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
        return sync::en_segundo_plano(&ruta, || abrir_con_modelo(c));
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
            Ok(())
        }
        Some(AccionSync::Init { url, eleccion }) => {
            let mut db = abrir(c)?;
            if sync::leer_config(&ruta)?.is_some() {
                bail!(tr!("sync is already set up here (`omoi sync status`; `omoi sync off` to start over)", "la sincronización ya está configurada aquí (`omoi sync status`; `omoi sync off` para empezar de nuevo)"));
            }
            let e = elegir(&eleccion)?;
            let config = sync::config_de(&url, &e);
            let (g, p, np) = sync::resumen_de_subida(&db, &config)?;
            println!(
                "{}",
                tr!(
                    "Will upload {g} global memories and {p} from {np} project(s), encrypted (preferences: {}). Everything stays on this computer too.",
                    "Se subirán {g} memorias globales y {p} de {np} proyecto(s), cifradas (preferencias: {}). Todo sigue también en este equipo.",
                    if e.preferencias { tr!("yes", "sí") } else { "no".into() }
                )
            );
            if !eleccion.yes && std::io::stdin().is_terminal() && !preguntar_si(&tr!("Continue?", "¿Seguir?"), true)? {
                println!("{}", tr!("nothing changed", "no se ha cambiado nada"));
                return Ok(());
            }
            sync::iniciar(&mut db, &ruta, &config)?;
            drop(db);
            primera(&tr!(
                "Sync set up. To add another computer, run `omoi sync code` here and `omoi sync join <code>` there.",
                "Sincronización configurada. Para añadir otro equipo: `omoi sync code` aquí y `omoi sync join <código>` allí."
            ))
        }
        Some(AccionSync::Join { codigo, url: otra_url, eleccion }) => {
            let mut db = abrir(c)?;
            if sync::leer_config(&ruta)?.is_some() {
                bail!(tr!("sync is already set up here (`omoi sync off` to start over)", "la sincronización ya está configurada aquí (`omoi sync off` para empezar de nuevo)"));
            }
            let (url, clave) = sync::leer_codigo(&codigo)?;
            let url = otra_url.unwrap_or(url);
            let e = elegir(&eleccion)?;
            let config = sync::config_de(&url, &e);
            sync::unirse(&mut db, &ruta, &config, &clave)?;
            drop(db);
            primera(&tr!("Joined {}.", "Unido a {}.", url))
        }
        Some(AccionSync::Code) => {
            println!("{}", sync::codigo(&ruta)?);
            eprintln!("{}", tr!("It carries the key to your memories: share it only through a private channel.", "Lleva la clave de tus memorias: pásalo solo por un canal privado."));
            Ok(())
        }
        Some(AccionSync::Status) => {
            let config = sync::leer_config(&ruta)?;
            if c.json {
                return json(&serde_json::json!({
                    "config": config,
                    "state": sync::leer_estado(&ruta),
                    "pending": sync::pendiente(&ruta),
                }));
            }
            println!("{}", estado_sync(&ruta));
            if let Some(cf) = config {
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
        Some(AccionSync::Off { yes }) => {
            let db = abrir(c)?;
            if sync::leer_config(&ruta)?.is_none() {
                println!("{}", tr!("sync is not set up", "la sincronización no está configurada"));
                return Ok(());
            }
            if !yes {
                anyhow::ensure!(std::io::stdin().is_terminal(), tr!("add --yes to stop syncing", "añade --yes para dejar de sincronizar"));
                if !preguntar_si(&tr!("Stop syncing on this computer? Its memories stay; the key is deleted here.", "¿Dejar de sincronizar en este equipo? Sus memorias se quedan; la clave se borra de aquí."), false)? {
                    return Ok(());
                }
            }
            sync::desactivar(&db, &ruta)?;
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
            ("kind", "fact, decision, gotcha o preference."),
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
        ]),
        ("forget", "Borra una memoria y su historial. No se puede deshacer.", &[
            ("id", "Id de la memoria (basta con el principio)."),
            ("yes", "Sin preguntar. Sin él, omoi pregunta en una terminal y se niega en cualquier otro sitio."),
        ]),
        ("history", "Versiones anteriores de una memoria.", &[("id", "Id de la memoria (basta con el principio).")]),
        ("recent", "Las últimas memorias guardadas o cambiadas.", &[
            ("limit", "Cuántas."),
            ("kind", "Solo de este tipo."),
            ("all", "De todos los proyectos."),
        ]),
        ("projects", "Proyectos con memorias.", &[]),
        ("export", "Todas las memorias con su historial, una por línea en JSON.", &[(
            "all",
            "De todos los proyectos (por defecto, este y las globales).",
        )]),
        ("tui", "Ver, buscar, fijar y olvidar memorias en la terminal.", &[("all", "Empezar con todos los proyectos (por defecto, este y las globales).")]),
        ("import", "Importa memorias de una exportación de engram (`engram export`) o de `omoi export`.", &[
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
        ]),
        ("recall", "Para los hooks de Claude Code: lee el JSON del hook y escribe las memorias que hay que inyectar.", &[(
            "session_start",
            "Al empezar la sesión (o tras compactar): preferencias y memorias fijadas.",
        )]),
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
            })
            .mut_subcommand("join", |s| {
                opciones_sync(s.about("Únete a un repositorio con el que ya sincroniza otro equipo, con el código de `omoi sync code`."))
                    .mut_arg("codigo", |a| a.help("El código que escribe `omoi sync code`.").value_name("CÓDIGO"))
                    .mut_arg("url", |a| a.help("Llegar al mismo repositorio por otra URL (por ejemplo, SSH aquí y HTTPS en el otro equipo)."))
            })
            .mut_subcommand("code", |s| s.about("Escribe el código para unirse desde otro equipo. Lleva la clave: pásalo por un canal privado."))
            .mut_subcommand("status", |s| s.about("Cómo está configurada la sincronización y cómo fue la última."))
            .mut_subcommand("off", |s| s.about("Deja de sincronizar en este equipo (sus memorias se quedan).").mut_arg("yes", |a| a.help("No preguntar.")))
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

/// En una terminal, pregunta; en cualquier otro sitio (un agente, un script) se niega sin `--yes`,
/// para que nadie borre por no haber leído la ayuda.
fn confirmar_olvido(m: &Memoria) -> Result<bool> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        bail!(tr!(
            "forget can't be undone: add --yes to delete {} without asking",
            "forget no se puede deshacer: añade --yes para borrar {} sin preguntar",
            corto(&m.id)
        ));
    }
    eprint!(
        "{}",
        tr!(
            "Forget «{}» and its history? It can't be undone [y/N] ",
            "¿Olvidar «{}» y su historial? No se puede deshacer [s/N] ",
            m.title
        )
    );
    std::io::stderr().flush()?;
    let mut linea = String::new();
    std::io::stdin().read_line(&mut linea)?;
    Ok(matches!(linea.trim().to_lowercase().as_str(), "y" | "yes" | "s" | "si" | "sí"))
}
