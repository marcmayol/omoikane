//! `omoi`: memoria local para agentes de IA. Un binario, un fichero SQLite.

use anyhow::{Context, Result, bail};
use clap::{Args, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use omoikane::buscar::{self, Opciones};
use omoikane::memoria::{self, Ambito, Cambios, Memoria, Nueva, Tipo, Version};
use omoikane::{db, idioma, proyecto, tr};
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
    },
    /// Search the memories of this project and the global ones.
    Search {
        /// What you are looking for, in plain words.
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        #[arg(short, long)]
        kind: Option<Tipo>,
        /// Only memories with this tag (repeatable).
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// In every project.
        #[arg(long)]
        all: bool,
    },
    /// Show a whole memory (the start of the id is enough).
    Get { id: String },
    /// Change a memory. The previous version is kept in its history.
    Update {
        id: String,
        #[arg(short, long)]
        title: Option<String>,
        /// New content ("-" to read it from standard input).
        #[arg(short, long)]
        content: Option<String>,
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
        #[arg(long, conflicts_with = "unpin")]
        pin: bool,
        #[arg(long)]
        unpin: bool,
        /// Why it changes (kept with the previous version).
        #[arg(short, long)]
        reason: Option<String>,
    },
    /// Delete a memory and its history. It can't be undone.
    Forget { id: String },
    /// Previous versions of a memory.
    History { id: String },
    /// The latest saved or changed memories.
    Recent {
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
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
    /// Check SQLite, FTS5 and sqlite-vec, and show where the database is.
    Doctor,
}

fn main() -> Result<()> {
    let cli = Cli::from_arg_matches(&en_espanol(Cli::command()).get_matches()).unwrap_or_else(|e| e.exit());
    let c = &cli.comun;
    match cli.orden {
        Orden::Doctor => doctor(c),
        Orden::Remember { kind, title, content, paths, tags, pinned, global, source } => {
            let project = if global { String::new() } else { proyecto_de(c)? };
            let content = texto_o_entrada(content)?;
            let m = memoria::guardar(
                &mut abrir(c)?,
                Nueva { project, kind: Some(kind), title, content, paths, tags, pinned, source },
            )?;
            if c.json {
                return json(&m);
            }
            println!("{}", tr!("saved {}", "guardada {}", corto(&m.id)));
            Ok(())
        }
        Orden::Search { query, kind, tags, limit, all } => {
            let o = Opciones { ambito: ambito(c, all)?, kind, tags, limite: limit };
            let res = buscar::buscar(&mut abrir(c)?, &query.join(" "), &o)?;
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
        Orden::Update { id, title, content, kind, paths, no_paths, tags, no_tags, pin, unpin, reason } => {
            let cambios = Cambios {
                title,
                content: content.map(|t| texto_o_entrada(Some(t))).transpose()?,
                kind,
                paths: if no_paths { Some(Vec::new()) } else { (!paths.is_empty()).then_some(paths) },
                tags: if no_tags { Some(Vec::new()) } else { (!tags.is_empty()).then_some(tags) },
                pinned: if pin { Some(true) } else if unpin { Some(false) } else { None },
                reason,
            };
            let m = memoria::actualizar(&mut abrir(c)?, &id, cambios)?;
            if c.json {
                return json(&m);
            }
            println!("{}", tr!("updated {}", "actualizada {}", corto(&m.id)));
            Ok(())
        }
        Orden::Forget { id } => {
            let m = memoria::olvidar(&mut abrir(c)?, &id)?;
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
        Orden::Export { all } => {
            let mut salida = std::io::stdout().lock();
            let n = memoria::exportar(&abrir(c)?, &ambito(c, all)?, &mut salida)?;
            salida.flush()?;
            eprintln!("{}", tr!("{n} memories exported", "{n} memorias exportadas"));
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
    Ok(if todos { Ambito::Todos } else { Ambito::Proyecto(proyecto_de(c)?) })
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
    if let Ok(p) = proyecto_de(c) {
        println!("{}{p}", tr!("project    ", "proyecto   "));
    }
    println!("{}", tr!("language   en (OMOIKANE_LANG=es for Spanish)", "idioma     es (OMOIKANE_LANG)"));
    Ok(())
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
    let textos: &[(&str, &str, &[(&str, &str)])] = &[
        ("remember", "Guarda una memoria. El contenido va como argumento o por la entrada estándar.", &[
            ("kind", "fact, decision, gotcha o preference."),
            ("title", "Una línea que diga de qué va."),
            ("content", "Qué, por qué y dónde. Sin él (o con «-»), se lee de la entrada estándar."),
            ("paths", "Fichero o carpeta del proyecto al que se refiere (se puede repetir)."),
            ("tags", "Etiqueta libre (se puede repetir)."),
            ("pinned", "Se inyecta siempre en su proyecto."),
            ("global", "Vale para todos los proyectos, no solo para este."),
            ("source", "Quién la guarda (por ejemplo «claude-code:<sesión>»)."),
        ]),
        ("search", "Busca en las memorias de este proyecto y en las globales.", &[
            ("query", "Lo que buscas, en lenguaje normal."),
            ("tags", "Solo las que llevan esta etiqueta (se puede repetir)."),
            ("all", "En todos los proyectos."),
        ]),
        ("get", "Una memoria entera (basta con el principio del id).", &[]),
        ("update", "Cambia una memoria. La versión anterior queda en el historial.", &[
            ("content", "Contenido nuevo («-» para leerlo de la entrada estándar)."),
            ("paths", "Sustituye las rutas por estas (se puede repetir)."),
            ("no_paths", "Quita todas las rutas."),
            ("tags", "Sustituye las etiquetas por estas (se puede repetir)."),
            ("no_tags", "Quita todas las etiquetas."),
            ("reason", "Por qué cambia (se guarda con la versión anterior)."),
        ]),
        ("forget", "Borra una memoria y su historial. No se puede deshacer.", &[]),
        ("history", "Versiones anteriores de una memoria.", &[]),
        ("recent", "Las últimas memorias guardadas o cambiadas.", &[("all", "De todos los proyectos.")]),
        ("projects", "Proyectos con memorias.", &[]),
        ("export", "Todas las memorias con su historial, una por línea en JSON.", &[(
            "all",
            "De todos los proyectos (por defecto, este y las globales).",
        )]),
        ("doctor", "Comprueba SQLite, FTS5 y sqlite-vec, y dice dónde está la base.", &[]),
    ];
    textos.iter().fold(c, |c, (orden, about, args)| {
        c.mut_subcommand(*orden, |s| args.iter().fold(s.about(*about), |s, (arg, ayuda)| s.mut_arg(*arg, |a| a.help(*ayuda))))
    })
}
